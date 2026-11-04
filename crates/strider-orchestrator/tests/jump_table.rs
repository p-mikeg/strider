//! Jump-table recognition, seating and arm resolution.
//!
//! One binary over `jump_table/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "jump_table/chained_tables_resolve_one_level_per_round.rs"]
mod chained_tables_resolve_one_level_per_round;

#[path = "jump_table/direct_edge_outranks_a_seeded_arm.rs"]
mod direct_edge_outranks_a_seeded_arm;

#[path = "jump_table/field_offset_in_a_mapped_first_page.rs"]
mod field_offset_in_a_mapped_first_page;

#[path = "jump_table/interworking_table_widens_in_arm_mode.rs"]
mod interworking_table_widens_in_arm_mode;

#[path = "jump_table/jump_table_byte_cell.rs"]
mod jump_table_byte_cell;

#[path = "jump_table/jump_table_field_index.rs"]
mod jump_table_field_index;

#[path = "jump_table/jump_table_field_test_guard.rs"]
mod jump_table_field_test_guard;

#[path = "jump_table/jump_table_index_reloaded_past_a_store.rs"]
mod jump_table_index_reloaded_past_a_store;

#[path = "jump_table/jump_table_lifting.rs"]
mod jump_table_lifting;

#[path = "jump_table/jump_table_rotate_index.rs"]
mod jump_table_rotate_index;

#[path = "jump_table/jump_table_sibling_extend_guard.rs"]
mod jump_table_sibling_extend_guard;

#[path = "jump_table/jump_table_truncated_byte.rs"]
mod jump_table_truncated_byte;

#[path = "jump_table/masked_scaled_narrow_index.rs"]
mod masked_scaled_narrow_index;

#[path = "jump_table/offset_index_is_bounded_before_the_offset.rs"]
mod offset_index_is_bounded_before_the_offset;

#[path = "jump_table/oscillating_site_is_withdrawn.rs"]
mod oscillating_site_is_withdrawn;

#[path = "jump_table/ppc32_got2_jump_table.rs"]
mod ppc32_got2_jump_table;

#[path = "jump_table/seated_arm_loss.rs"]
mod seated_arm_loss;

#[path = "jump_table/table_seat_rules.rs"]
mod table_seat_rules;

#[path = "jump_table/thumb_byte_table_unbounded_index.rs"]
mod thumb_byte_table_unbounded_index;

#[path = "jump_table/trampoline_chain_depth.rs"]
mod trampoline_chain_depth;

#[path = "jump_table/widened_mode_committing_table.rs"]
mod widened_mode_committing_table;

#[path = "jump_table/withdrawn_seat_is_not_republished.rs"]
mod withdrawn_seat_is_not_republished;
