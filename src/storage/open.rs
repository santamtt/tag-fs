use pub_fields::pub_fields;

#[pub_fields]
#[derive(Debug, Clone, Copy)]
pub struct OpenEntry {
    id: u128,
    offset: u64,
    generation: u64,
    flags: u32,
    refcount: u32
}

impl OpenEntry {
    pub fn new() -> Self {
        Self {
            id: 0,
            offset: 0,
            generation: 0,
            flags: 0,
            refcount: 0 
        }
    }
}
