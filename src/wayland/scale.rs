#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct Scale(pub i32);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Logical(pub i32, pub i32);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Physical(pub i32, pub i32);

impl Scale {
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / 120.0
    }

    pub fn round_up(self) -> i32 {
        (self.0 + 119) / 120
    }
}

impl Logical {
    pub fn to_physical(self, scale: Scale) -> Physical {
        Physical((self.0 * scale.0 + 60) / 120, (self.1 * scale.0 + 60) / 120)
    }
}

impl Physical {
    pub fn size(self) -> (i32, i32) {
        (self.0, self.1)
    }
}
