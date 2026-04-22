use anchor_lang::prelude::*;

declare_id!("7SD4qajisBhtZbe2xMRjaBpyLkGVJD5qd6vXznHF28EP");

#[program]
pub mod mercs_and_mines {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        msg!("Greetings from: {:?}", ctx.program_id);
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize {}
