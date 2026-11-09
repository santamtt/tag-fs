use std::io::{ Result };
use std::cmp::{ Ordering };

use pub_fields::pub_fields;
use serde::{ Serialize, Deserialize };

use crate::storage::lst::*;
use crate::storage::data_structures::mem_arena::*;

#[pub_fields]
#[repr(C,align(64))]
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct SegmentHeader {
    n_tree: u64,
    free_size: u64,
    checksum: u64
}

#[derive(Debug, Copy, Clone)]
struct SegTreeNode {
    id: u64,
    meta: SegmentHeader,
    left: Option<usize>,
    right: Option<usize>
}

pub enum SegTreeSearch {
    Empty,
    Full,
    Pos(u64)
}

#[derive(Debug)]
pub struct SegTree {
    tree: Arena<SegTreeNode>,
    root: Option<usize>
}

impl SegTree {

    pub fn new() -> Self {
        Self {
            tree: Arena::new(),
            root: Option::None
        }
    }

    fn _get(&mut self, size: u64) -> Option<SegTreeNode> {
        if self.root.is_none() { return Option::None; }
        
        let mut root_pos = self.root.unwrap();

        loop {
            match self.tree.elements[root_pos] {
                Some(node) => {
                    
                    let is_left: bool = node.meta.free_size >= size;
                    let opt_next = if is_left { node.left } else { node.right };

                    if opt_next.is_none() { break; }

                    root_pos = opt_next.unwrap();
                }, None => {
                    unreachable!("called unreachable here in file {} at line {}", file!(), line!());
                }
            }
        }

        self.tree.elements[root_pos]
    }

    pub fn select_segment(&mut self, size: u64) -> SegTreeSearch {
        let mut select: SegTreeSearch = SegTreeSearch::Full;

        match self._get(size) {
            Some(node) => {
                if node.meta.free_size >= size {
                    select = SegTreeSearch::Pos(node.id);
                }
            }, None => {
                select = SegTreeSearch::Empty
            }
        }

        select
    }

    pub fn insert(&mut self, id: u64, header: SegmentHeader) { 
        match self._get(header.free_size) {

            Some(mut node) => {
                let is_left: bool = node.meta.free_size >= header.free_size;

                let new_node: SegTreeNode = SegTreeNode {
                    id: id,
                    meta: header,
                    left: Option::None,
                    right: Option::None
                };

                let update_pos: usize = self.tree.insert(new_node);
                if is_left { node.left = Option::Some(update_pos); } else { 
                    node.right = Option::Some(update_pos); 
                }

            }, None => {
                let root_node: SegTreeNode = SegTreeNode {
                    id: id,
                    meta: header,
                    left: Option::None,
                    right: Option::None
                };

                let alloc_root: usize = self.tree.insert(root_node);
                self.root = Option::Some(alloc_root);
            }
        }
    }
}
