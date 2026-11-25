//! Free-space-ordered segment tree (originally `storage::segment::SegTree`).
//!
//! Preserved as a standalone data structure: the allocator in
//! `storage::segment` now uses `BTreeMap`/`BTreeSet` instead. The node
//! metadata type was renamed from `SegmentHeader` to `SegmentMeta` so this file
//! no longer depends on the on-disk segment header.
//!
//! Known limitations (kept as-is on purpose):
//! - no rebalancing yet (rotations / heights are still to be implemented);
//! - `update_free_space` changes the ordering key in place without
//!   re-inserting the node, which can break the BST invariant.
#![allow(dead_code, unused_imports, unused_variables, unused_mut)]

use std::io::{ Result };

use pub_fields::pub_fields;
use serde::{ Serialize, Deserialize };

use crate::storage::data_structures::mem_arena::*;

use core::mem::swap;

#[pub_fields]
#[repr(C,align(4096))]
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct SegmentMeta {
    n_tree: u64,
    free_size: u64,
    free_pos: u64,
    checksum: u64
}

#[derive(Debug, Copy, Clone)]
struct SegTreeNode {
    id: u64,
    meta: SegmentMeta,
    left: Option<usize>,
    right: Option<usize>
}

pub enum SegTreeSearch {
    Empty,
    Full,
    Pos(usize, u64, u64)
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

    fn _get(&mut self, size: u64) -> (usize, Option<SegTreeNode>) {
        if self.root.is_none() { return (0, Option::None); }
        
        let mut root_pos = self.root.unwrap();

        loop {
            match self.tree.elements[root_pos].as_ref() {
                Some(ref node) => {
                    
                    let is_left: bool = node.meta.free_size < size;
                    let opt_next = if is_left { node.left } else { node.right };

                    if opt_next.is_none() { break; }

                    root_pos = opt_next.unwrap();
                }, None => {
                    unreachable!("called unreachable here in file {} at line {}", file!(), line!());
                }
            }
        }

        (root_pos, self.tree.elements[root_pos])
    }

    pub fn update_free_space(&mut self, node_pos: usize, alloc_space: u64) {
        match self.tree.elements[node_pos].as_mut() {
            Some(ref mut node) => {
                node.meta.free_size -= alloc_space;
                node.meta.free_pos += 1;
            },
            _ => {}
        }
    }

    pub fn select_segment(&mut self, size: u64) -> SegTreeSearch {
        let mut select: SegTreeSearch = SegTreeSearch::Full;
        let mut get_res = self._get(size);

        match get_res.1.as_ref() {
            Some(ref node) => {
                if node.meta.free_size >= size {
                    select = SegTreeSearch::Pos(get_res.0, node.id, node.meta.free_pos);
                }
            }, None => {
                select = SegTreeSearch::Empty
            }
        }

        select
    }

    pub fn insert(&mut self, id: u64, header: SegmentMeta) { 
        let mut get_res = self._get(header.free_size);
        match get_res.1.as_mut() {

            Some(ref mut node) => {
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
