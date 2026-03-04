use anchor_lang::{
    prelude::*,
    solana_program::sysvar::instructions::{
        load_current_index_checked, load_instruction_at_checked, ID as INSTRUCTIONS_SYSVAR_ID,
    },
    Discriminator,
};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{transfer, Mint, Token, TokenAccount, Transfer},
};

declare_id!("8B4sLttXDiq1KpGP4sgw2ZvTh81SWQhkzSrtvPYPSXVc");

/// 闪电贷程序：
/// 1. `borrow` 先把协议资金借给用户
/// 2. 通过指令内省强制要求交易最后一条必须是 `repay`
/// 3. `repay` 从交易第一条借款指令中读取借款额并加上手续费归还
#[program]
pub mod codex_anchor_flash_loan {
    use super::*;

    /// 借款指令：
    /// - 从协议金库 ATA 转账到借款人 ATA
    /// - 验证本交易最后一条指令必须是本程序的 `repay`
    pub fn borrow(ctx: Context<Loan>, borrow_amount: u64) -> Result<()> {
        // 避免无效金额（0）进入逻辑。
        require!(borrow_amount > 0, ProtocolError::InvalidAmount);

        // 通过 PDA seeds 派生出协议账户签名，让程序可以代表 protocol PDA 调用 Token Program。
        let seeds = &[b"protocol".as_ref(), &[ctx.bumps.protocol]];
        let signer_seeds = &[&seeds[..]];

        // 把协议资金借给借款人。
        transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.protocol_ata.to_account_info(),
                    to: ctx.accounts.borrower_ata.to_account_info(),
                    authority: ctx.accounts.protocol.to_account_info(),
                },
                signer_seeds,
            ),
            borrow_amount,
        )?;

        /*
           指令内省（Instruction Introspection）：
           借款时直接检查交易末尾是否存在合法 repay，确保“借款与还款同交易原子完成”。
           若末尾不是 repay，整个交易会失败并回滚，协议不会实际放出可被带走的资金。
        */
        let ixs = ctx.accounts.instructions.to_account_info();

        // 当前 borrow 必须是交易中的第一条指令（索引 0）。
        // 这是为了配合 repay 从 index=0 读取借款金额，防止交易结构被绕过。
        let current_index = load_current_index_checked(&ixs)?;
        require_eq!(current_index, 0, ProtocolError::InvalidIx);

        // instruction sysvar 前两个字节是交易里指令数量（u16, little-endian）。
        let instruction_sysvar = ixs.try_borrow_data()?;
        let len = u16::from_le_bytes(
            instruction_sysvar[0..2]
                .try_into()
                .map_err(|_| ProtocolError::InvalidInstructionIndex)?,
        );
        require!(len > 0, ProtocolError::InvalidInstructionIndex);

        // 读取最后一条指令并验证其为本程序的 repay。
        if let Ok(repay_ix) = load_instruction_at_checked(len as usize - 1, &ixs) {
            // 必须是本程序 ID。
            require_keys_eq!(repay_ix.program_id, ID, ProtocolError::InvalidProgram);

            // 指令数据前 8 字节必须是 Repay 的 discriminator。
            require!(
                repay_ix.data.len() >= 8,
                ProtocolError::InvalidInstructionIndex
            );
            require!(
                repay_ix.data[0..8].eq(instruction::Repay::DISCRIMINATOR),
                ProtocolError::InvalidIx
            );

            // repay 的账户布局与 Loan 结构一致：
            // [0] borrower, [1] protocol, [2] mint, [3] borrower_ata, [4] protocol_ata, ...
            // 这里核对两个 ATA，确保还款回到本次借款对应账户，防止“还到别处”。
            require_keys_eq!(
                repay_ix
                    .accounts
                    .get(3)
                    .ok_or(ProtocolError::InvalidBorrowerAta)?
                    .pubkey,
                ctx.accounts.borrower_ata.key(),
                ProtocolError::InvalidBorrowerAta
            );
            require_keys_eq!(
                repay_ix
                    .accounts
                    .get(4)
                    .ok_or(ProtocolError::InvalidProtocolAta)?
                    .pubkey,
                ctx.accounts.protocol_ata.key(),
                ProtocolError::InvalidProtocolAta
            );
        } else {
            return Err(ProtocolError::MissingRepayIx.into());
        }

        Ok(())
    }

    /// 还款指令：
    /// - 从交易第一条 borrow 指令读取借款额
    /// - 计算手续费（500 bps = 5%）
    /// - 从借款人 ATA 转回协议 ATA
    pub fn repay(ctx: Context<Loan>) -> Result<()> {
        let ixs = ctx.accounts.instructions.to_account_info();

        // 从 index=0 读取 borrow 指令数据中的借款金额（data[8..16]）。
        // 前 8 字节是 borrow 指令 discriminator，后 8 字节是 borrow_amount(u64 LE)。
        let mut amount_borrowed: u64;
        if let Ok(borrow_ix) = load_instruction_at_checked(0, &ixs) {
            require!(
                borrow_ix.data.len() >= 16,
                ProtocolError::InvalidInstructionIndex
            );

            let mut borrowed_data: [u8; 8] = [0u8; 8];
            borrowed_data.copy_from_slice(&borrow_ix.data[8..16]);
            amount_borrowed = u64::from_le_bytes(borrowed_data);
        } else {
            return Err(ProtocolError::MissingBorrowIx.into());
        }

        // 费用固定为 500 bps（5%），使用 checked 计算防止溢出。
        let fee = (amount_borrowed as u128)
            .checked_mul(500)
            .ok_or(ProtocolError::Overflow)?
            .checked_div(10_000)
            .ok_or(ProtocolError::Overflow)? as u64;
        amount_borrowed = amount_borrowed
            .checked_add(fee)
            .ok_or(ProtocolError::Overflow)?;

        // 从借款人账户归还“本金 + 手续费”到协议金库。
        transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.borrower_ata.to_account_info(),
                    to: ctx.accounts.protocol_ata.to_account_info(),
                    authority: ctx.accounts.borrower.to_account_info(),
                },
            ),
            amount_borrowed,
        )?;

        Ok(())
    }
}

/// 借款与还款共用同一账户集合。
#[derive(Accounts)]
pub struct Loan<'info> {
    /// 借款人，负责签名；若 borrower_ata 不存在，由该账户支付 ATA 创建费用。
    #[account(mut)]
    pub borrower: Signer<'info>,

    /// 协议 PDA（种子固定为 "protocol"），作为协议流动性金库的 ATA 权限拥有者。
    #[account(
        seeds = [b"protocol".as_ref()],
        bump,
    )]
    pub protocol: SystemAccount<'info>,

    /// 被借代币的 Mint。
    pub mint: Account<'info, Mint>,

    /// 借款人的 ATA：借款时接收资金，还款时转出资金。
    #[account(
        init_if_needed,
        payer = borrower,
        associated_token::mint = mint,
        associated_token::authority = borrower,
    )]
    pub borrower_ata: Account<'info, TokenAccount>,

    /// 协议金库 ATA：借款时转出，还款时转入。
    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = protocol,
    )]
    pub protocol_ata: Account<'info, TokenAccount>,

    /// 指令 sysvar，用于读取同一交易中的其他指令数据。
    #[account(address = INSTRUCTIONS_SYSVAR_ID)]
    /// CHECK: 仅做固定地址校验并以 sysvar 方式读取，不反序列化业务数据。
    pub instructions: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// 协议错误定义：尽量细分，便于定位失败原因。
#[error_code]
pub enum ProtocolError {
    #[msg("Invalid instruction")]
    InvalidIx,
    #[msg("Invalid instruction index")]
    InvalidInstructionIndex,
    #[msg("Invalid amount")]
    InvalidAmount,
    #[msg("Not enough funds")]
    NotEnoughFunds,
    #[msg("Program Mismatch")]
    ProgramMismatch,
    #[msg("Invalid program")]
    InvalidProgram,
    #[msg("Invalid borrower ATA")]
    InvalidBorrowerAta,
    #[msg("Invalid protocol ATA")]
    InvalidProtocolAta,
    #[msg("Missing repay instruction")]
    MissingRepayIx,
    #[msg("Missing borrow instruction")]
    MissingBorrowIx,
    #[msg("Overflow")]
    Overflow,
}
