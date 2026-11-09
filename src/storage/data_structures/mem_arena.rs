#[derive(Debug)]
pub struct Arena<T> {
    pub elements: Vec<Option<T>>,
    free: Vec<usize>
}

impl<T> Arena<T> {
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
            free: Vec::new()
        }
    }

    pub fn delete(&mut self, pos: usize) -> bool { 
        if self.elements.len() <= pos { return false; }

        self.elements[pos] = Option::None;
        self.free.push(pos); 

        true
    }

    pub fn insert(&mut self, elem_in: T) -> usize {

        let alloc_pos: usize = 
            match self.free.pop() {
                Some(pos) => {
                    self.elements[pos] = Option::Some(elem_in);
                    pos
                },

                None => {
                    self.elements.push(Option::Some(elem_in)); 
                    self.elements.len() - 1
                }
            };

        alloc_pos 
    }

    pub fn check_length(&self, pos: usize) -> bool { pos < self.elements.len() }

}
