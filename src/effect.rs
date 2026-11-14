#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectClass {
    None,
    Idempotent,
    External,
}

impl EffectClass {
    pub const ALL: [Self; 3] = [Self::None, Self::Idempotent, Self::External];

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Idempotent => "idempotent",
            Self::External => "external",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.name() == name)
    }

    pub const fn undeclared() -> Self {
        Self::External
    }

    pub const fn may_be_unknown(self) -> bool {
        matches!(self, Self::External)
    }
}
