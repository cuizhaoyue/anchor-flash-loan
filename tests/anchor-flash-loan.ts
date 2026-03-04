import * as anchor from "@coral-xyz/anchor";
import { AnchorError, BN, Program } from "@coral-xyz/anchor";
import { expect } from "chai";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  createMint,
  getAccount,
  getOrCreateAssociatedTokenAccount,
  mintTo,
} from "@solana/spl-token";
import {
  Keypair,
  PublicKey,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  SystemProgram,
  Transaction,
} from "@solana/web3.js";
import { AnchorFlashLoan } from "../target/types/anchor_flash_loan";

describe("anchor-flash-loan", () => {
  // 使用 Anchor.toml 中配置的钱包和本地集群作为测试提供者。
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.getProvider() as anchor.AnchorProvider;
  // provider 钱包的 payer，用于创建 mint、ATA 并铸币。
  const payer = (provider.wallet as anchor.Wallet & { payer: Keypair }).payer;
  const program = anchor.workspace.AnchorFlashLoan as Program<AnchorFlashLoan>;

  // 测试参数：借款 100_000，手续费 500 bps = 5%。
  const BORROW_AMOUNT = 100_000n;
  const FEE_BPS = 500n;
  const INITIAL_PROTOCOL_LIQUIDITY = 1_000_000n;
  const INITIAL_BORROWER_BALANCE = 20_000n;

  type Fixture = {
    borrower: Keypair;
    protocolPda: PublicKey;
    mint: PublicKey;
    borrowerAta: PublicKey;
    protocolAta: PublicKey;
    strangerAta: PublicKey;
  };

  async function setupFixture(): Promise<Fixture> {
    // 每个用例使用独立 borrower，避免状态互相污染。
    const borrower = Keypair.generate();
    const airdropSig = await provider.connection.requestAirdrop(
      borrower.publicKey,
      2 * anchor.web3.LAMPORTS_PER_SOL,
    );
    await provider.connection.confirmTransaction(airdropSig, "confirmed");

    // 创建测试 mint（6 位精度），由 payer 作为 mint authority。
    const mint = await createMint(
      provider.connection,
      payer,
      payer.publicKey,
      null,
      6,
    );

    // 协议 PDA，与合约内 seeds = ["protocol"] 对齐。
    const [protocolPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("protocol")],
      program.programId,
    );

    // 协议金库 ATA（owner 是 PDA，所以允许 off-curve）。
    const protocolAta = (
      await getOrCreateAssociatedTokenAccount(
        provider.connection,
        payer,
        mint,
        protocolPda,
        true,
      )
    ).address;

    // 借款人 ATA：接收借款并在 repay 时归还。
    const borrowerAta = (
      await getOrCreateAssociatedTokenAccount(
        provider.connection,
        payer,
        mint,
        borrower.publicKey,
      )
    ).address;

    // 构造一个“错误 ATA”，用于触发 InvalidBorrowerAta 负例。
    const strangerAta = (
      await getOrCreateAssociatedTokenAccount(
        provider.connection,
        payer,
        mint,
        Keypair.generate().publicKey,
      )
    ).address;

    // 预置协议流动性，供 borrow 转出。
    await mintTo(
      provider.connection,
      payer,
      mint,
      protocolAta,
      payer,
      INITIAL_PROTOCOL_LIQUIDITY,
    );

    // 给 borrower 预置少量余额，用于支付手续费差额。
    await mintTo(
      provider.connection,
      payer,
      mint,
      borrowerAta,
      payer,
      INITIAL_BORROWER_BALANCE,
    );

    return {
      borrower,
      protocolPda,
      mint,
      borrowerAta,
      protocolAta,
      strangerAta,
    };
  }

  function loanAccounts(fixture: Fixture, borrowerAtaOverride?: PublicKey) {
    // 统一构造 borrow/repay 所需账户，减少重复并避免漏参。
    return {
      borrower: fixture.borrower.publicKey,
      protocol: fixture.protocolPda,
      mint: fixture.mint,
      borrowerAta: borrowerAtaOverride ?? fixture.borrowerAta,
      protocolAta: fixture.protocolAta,
      instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
      tokenProgram: TOKEN_PROGRAM_ID,
      associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
    };
  }

  function parseErrorCode(error: unknown): string | undefined {
    // Anchor 在不同版本/错误路径下字段结构略有差异，这里做兼容解析。
    const e = error as {
      error?: { errorCode?: { code?: string } };
      errorCode?: { code?: string };
      logs?: string[];
    };
    const direct = e.error?.errorCode?.code ?? e.errorCode?.code;
    if (direct) {
      return direct;
    }
    if (Array.isArray(e.logs)) {
      const parsed = AnchorError.parse(e.logs);
      return parsed?.error?.errorCode?.code;
    }
    return undefined;
  }

  async function expectProgramError(
    promise: Promise<unknown>,
    expectedCode: string,
  ) {
    // 统一断言“应当失败且错误码匹配”，简化负例用例写法。
    try {
      await promise;
      expect.fail(`expected error code ${expectedCode}, but tx succeeded`);
    } catch (error) {
      const actualCode = parseErrorCode(error);
      expect(actualCode).to.equal(expectedCode);
    }
  }

  it("executes borrow + repay atomically and keeps 5% fee", async () => {
    // 1) 准备测试账户与初始资金。
    const fixture = await setupFixture();
    const protocolBefore = (await getAccount(provider.connection, fixture.protocolAta))
      .amount;
    const borrowerBefore = (await getAccount(provider.connection, fixture.borrowerAta))
      .amount;

    // 2) 在同一交易中按顺序组装 borrow + repay。
    const borrowIx = await program.methods
      .borrow(new BN(BORROW_AMOUNT.toString()))
      .accounts(loanAccounts(fixture))
      .instruction();
    const repayIx = await program.methods
      .repay()
      .accounts(loanAccounts(fixture))
      .instruction();

    // 3) 原子执行：任一步失败，整笔交易回滚。
    await provider.sendAndConfirm(
      new Transaction().add(borrowIx, repayIx),
      [fixture.borrower],
    );

    // 4) 校验净变化：协议增加 fee，借款人减少 fee。
    const protocolAfter = (await getAccount(provider.connection, fixture.protocolAta))
      .amount;
    const borrowerAfter = (await getAccount(provider.connection, fixture.borrowerAta))
      .amount;

    const fee = (BORROW_AMOUNT * FEE_BPS) / 10_000n;
    expect(protocolAfter).to.equal(protocolBefore + fee);
    expect(borrowerAfter).to.equal(borrowerBefore - fee);
  });

  it("rejects zero borrow amount", async () => {
    const fixture = await setupFixture();
    // borrow_amount = 0，应命中 ProtocolError::InvalidAmount。
    const borrowIx = await program.methods
      .borrow(new BN(0))
      .accounts(loanAccounts(fixture))
      .instruction();

    await expectProgramError(
      provider.sendAndConfirm(new Transaction().add(borrowIx), [fixture.borrower]),
      "InvalidAmount",
    );
  });

  it("rejects when borrow is not the first instruction", async () => {
    const fixture = await setupFixture();

    // 在 borrow 前插入一条系统转账，故意破坏“borrow 必须 index=0”的约束。
    const preIx = SystemProgram.transfer({
      fromPubkey: payer.publicKey,
      toPubkey: fixture.borrower.publicKey,
      lamports: 1,
    });
    const borrowIx = await program.methods
      .borrow(new BN(BORROW_AMOUNT.toString()))
      .accounts(loanAccounts(fixture))
      .instruction();
    const repayIx = await program.methods
      .repay()
      .accounts(loanAccounts(fixture))
      .instruction();

    await expectProgramError(
      provider.sendAndConfirm(
        new Transaction().add(preIx, borrowIx, repayIx),
        [fixture.borrower],
      ),
      "InvalidIx",
    );
  });

  it("rejects when repay instruction uses a mismatched borrower ATA", async () => {
    const fixture = await setupFixture();

    // borrow 使用正确 ATA，repay 改成 strangerAta，触发账户一致性校验失败。
    const borrowIx = await program.methods
      .borrow(new BN(BORROW_AMOUNT.toString()))
      .accounts(loanAccounts(fixture))
      .instruction();
    const repayIx = await program.methods
      .repay()
      .accounts(loanAccounts(fixture, fixture.strangerAta))
      .instruction();

    await expectProgramError(
      provider.sendAndConfirm(
        new Transaction().add(borrowIx, repayIx),
        [fixture.borrower],
      ),
      "InvalidBorrowerAta",
    );
  });
});
