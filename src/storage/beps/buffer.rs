#[derive(Debug, Clone, Copy)]
pub enum MessageType {
    Insert,
    Update,
    Delete,
    Get
}

#[derive(Debug)]
pub struct Message {
    pub msg_type: MessageType,
    pub key: u64,
    pub value: u64,
    pub _timestamp: u64
}

#[derive(Debug)]
pub struct Buffer {
    pub buffer: Vec<Message>,
    _size: usize
}

impl Buffer{
    pub fn new(_size_in: usize) -> Self {
        Self{
            buffer: Vec::new(),
            _size: _size_in
        }
    }

    pub fn insert(&mut self, msg_in: Message) -> bool {
        if self.buffer.len() >= self._size { return false; }
        self.buffer.push(msg_in);
        true
    }
}
