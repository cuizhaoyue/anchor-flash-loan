use anchor_lang::prelude::*;

declare_id!("8B4sLttXDiq1KpGP4sgw2ZvTh81SWQhkzSrtvPYPSXVc");

#[program]
pub mod anchor_flash_loan {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        msg!("Greetings from: {:?}", ctx.program_id);
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize {}
