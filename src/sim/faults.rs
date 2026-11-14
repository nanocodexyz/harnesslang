use crate::sim::rng::Rng;
use crate::target::Target;

pub const PROFILE_ROLLS: u64 = 10;
pub const CLEAN_ROLLS_END: u64 = 3;
pub const ERRORS_ROLLS_END: u64 = 6;
pub const ASYNC_ROLLS_END: u64 = 8;

pub const CRASH_PPM_MIN: u64 = 20_000;
pub const CRASH_PPM_MAX: u64 = 250_000;
pub const TORN_PPM_MAX: u64 = 300_000;
pub const UNSYNCED_CRASH_PPM_MAX: u64 = 300_000;
pub const POWER_LOSS_PPM_MIN: u64 = 300_000;
pub const POWER_LOSS_PPM_MAX: u64 = 1_000_000;
pub const SECTOR_PERSISTED_PPM: u64 = 500_000;
pub const LEASE_EXPIRY_PPM_MIN: u64 = 20_000;
pub const LEASE_EXPIRY_PPM_MAX: u64 = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerLoss {
    Never,
    Prefix { ppm: u64 },
    Sectors { ppm: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: &'static str,
    pub crash_ppm: u64,
    pub torn_ppm: u64,
    pub unsynced_crash_ppm: u64,
    pub power_loss: PowerLoss,
    pub lease_ppm: u64,
}

impl Profile {
    pub fn of(seed: u64, target: Target) -> Self {
        let mut rng = Rng::derive(seed, "profile");
        let roll = rng.below(PROFILE_ROLLS);
        let mut profile = Self {
            name: "clean",
            crash_ppm: 0,
            torn_ppm: 0,
            unsynced_crash_ppm: 0,
            power_loss: PowerLoss::Never,
            lease_ppm: 0,
        };
        if roll < CLEAN_ROLLS_END {
            return profile;
        }
        profile.crash_ppm = rng.range(CRASH_PPM_MIN, CRASH_PPM_MAX);
        profile.name = "errors";
        match target {
            Target::NativeJsonl => {
                profile.torn_ppm = rng.range(0, TORN_PPM_MAX);
                profile.unsynced_crash_ppm = rng.range(0, UNSYNCED_CRASH_PPM_MAX);
                if (ERRORS_ROLLS_END..ASYNC_ROLLS_END).contains(&roll) {
                    profile.name = "async";
                    profile.power_loss = PowerLoss::Prefix {
                        ppm: rng.range(POWER_LOSS_PPM_MIN, POWER_LOSS_PPM_MAX),
                    };
                } else if roll >= ASYNC_ROLLS_END {
                    profile.name = "lying";
                    profile.power_loss = PowerLoss::Sectors {
                        ppm: rng.range(POWER_LOSS_PPM_MIN, POWER_LOSS_PPM_MAX),
                    };
                }
            }
            Target::ServerPostgres => {
                if roll >= ERRORS_ROLLS_END {
                    profile.name = "lease";
                    profile.lease_ppm = rng.range(LEASE_EXPIRY_PPM_MIN, LEASE_EXPIRY_PPM_MAX);
                }
            }
            Target::NativeSqlite | Target::Cloudflare | Target::Vercel => {}
        }
        profile
    }
}
