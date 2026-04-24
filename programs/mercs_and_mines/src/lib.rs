use anchor_lang::prelude::*;

declare_id!("7SD4qajisBhtZbe2xMRjaBpyLkGVJD5qd6vXznHF28EP");

// ── Account structures ─────────────────────────────────────────────────────

/// Canonical PDA seeds: [b"player", wallet_pubkey]
/// Locked — changing seeds requires full program migration. See Wiki §4.
#[account]
#[derive(InitSpace)]
pub struct PlayerAccount {
    /// Player's Solana wallet (base58 pubkey). Used to re-derive the PDA.
    pub wallet: Pubkey,
    /// Trust Standing. Starts at 0 (Neutral). Range: −100 to +100.
    /// See The_Trust.md for the full standing threshold table.
    pub trust_standing: i32,
    /// Set to true at init. Server checks this before dispatching any
    /// Founding Courtesy transaction to avoid a redundant RPC call.
    pub founding_courtesy_claimed: bool,
    /// Stored bump avoids recalculating find_program_address on every CPI.
    pub bump: u8,
}

/// Canonical PDA seeds: [b"inventory", wallet_pubkey]
/// Locked — same migration caveat as PlayerAccount.
#[account]
#[derive(InitSpace)]
pub struct PlayerInventory {
    /// Player's Solana wallet (base58 pubkey). Used to re-derive the PDA.
    pub wallet: Pubkey,
    /// He3 balance in base units. Starts at 500 (Founding Courtesy delivery).
    pub he3_balance: u64,
    /// Stored bump.
    pub bump: u8,
}

// ── Instructions ───────────────────────────────────────────────────────────

#[program]
pub mod mercs_and_mines {
    use super::*;

    /// Founding Courtesy — one-time account initialization called by the
    /// game server on new player account creation. Initializes both PDAs and
    /// delivers 500 He3 to the player's inventory.
    ///
    /// The treasury (server keypair) pays all rent. The player wallet never
    /// signs or spends SOL. This is the Dungeon Master authority model.
    ///
    /// Calling this a second time for the same player_wallet will fail with
    /// a ConstraintSeeds / AccountAlreadyInitialized error — this is correct.
    pub fn founding_courtesy(ctx: Context<FoundingCourtesy>) -> Result<()> {
        let acct = &mut ctx.accounts.player_account;
        acct.wallet = ctx.accounts.player_wallet.key();
        acct.trust_standing = 0;
        acct.founding_courtesy_claimed = true;
        acct.bump = ctx.bumps.player_account;

        let inv = &mut ctx.accounts.player_inventory;
        inv.wallet = ctx.accounts.player_wallet.key();
        inv.he3_balance = 500;
        inv.bump = ctx.bumps.player_inventory;

        msg!(
            "Founding Courtesy: 500 He3 delivered to {}. Trust Standing: 0 (Neutral).",
            ctx.accounts.player_wallet.key()
        );
        Ok(())
    }
}

// ── Accounts context ───────────────────────────────────────────────────────

#[derive(Accounts)]
pub struct FoundingCourtesy<'info> {
    /// Player wallet pubkey — used only as a PDA seed.
    /// The player does not sign. The server (treasury) is the sole signer.
    ///
    /// CHECK: This account is never read from or written to. Its public key
    /// is the seed for player_account and player_inventory PDAs only.
    pub player_wallet: UncheckedAccount<'info>,

    #[account(
        init,
        payer = treasury,
        space = 8 + PlayerAccount::INIT_SPACE,
        seeds = [b"player", player_wallet.key().as_ref()],
        bump,
    )]
    pub player_account: Account<'info, PlayerAccount>,

    #[account(
        init,
        payer = treasury,
        space = 8 + PlayerInventory::INIT_SPACE,
        seeds = [b"inventory", player_wallet.key().as_ref()],
        bump,
    )]
    pub player_inventory: Account<'info, PlayerInventory>,

    /// The game server's treasury keypair. Pays all rent. Must be a signer.
    #[account(mut)]
    pub treasury: Signer<'info>,

    pub system_program: Program<'info, System>,
}
