//! Demonstrates the opt-in retained renderer dirty tree.
//!
//! `RendererDirtyTree` is the Rust-native counterpart to CC Ink's `markDirty`:
//! a leaf mutation dirties the leaf and ancestors, while measurement dirtiness
//! stays on the text-like leaf. Child order is retained to match CC Ink's
//! `childNodes` traversal. It is useful when building a custom retained
//! renderer around stable node identifiers.

use iocraft::prelude::*;

fn main() {
    let mut tree = RendererDirtyTree::<&'static str>::new();
    tree.register_root("root");
    tree.attach("messages", "root");
    tree.attach("row-42", "messages");
    tree.attach("prompt", "root");

    println!("root children: {:?}", tree.child_keys(&"root"));

    tree.mark_dirty(&"row-42", true);
    println!("row dirty: {}", tree.is_dirty(&"row-42"));
    println!("messages dirty: {}", tree.is_dirty(&"messages"));
    println!("root dirty: {}", tree.is_dirty(&"root"));
    println!("prompt dirty: {}", tree.is_dirty(&"prompt"));
    println!("row needs remeasure: {}", tree.is_measure_dirty(&"row-42"));

    let removed = tree.remove_subtree(&"messages");
    println!("removed subtree: {removed:?}");
    println!("root dirty after removal: {}", tree.is_dirty(&"root"));
}
