//! The builders the schema test runs each carry family over: every
//! column of every section spec, which names every column a smaller list
//! can.

use droppedneedle_kernel::db::statement::Choice;

use crate::export::sections::{ALL, Column, TableSection, Target};
use crate::import::carry::sql::{
    held_entry_insert, held_file_insert, link_count, link_insert, section_count, section_insert,
};

/// A section that lands in a v3 table.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TableTarget {
    section: &'static TableSection,
    table: &'static str,
}

impl Choice for TableTarget {
    fn all() -> Vec<Self> {
        ALL.iter()
            .copied()
            .filter_map(|section| match section.target {
                Target::Table(table) => Some(Self { section, table }),
                Target::AvatarFiles | Target::HeldFiles | Target::BundleOnly => None,
            })
            .collect()
    }
}

/// A library reference column of a section that lands in a v3 table.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LinkColumn {
    target: TableTarget,
    column: &'static Column,
}

impl Choice for LinkColumn {
    fn all() -> Vec<Self> {
        let mut all = Vec::new();
        for target in TableTarget::all() {
            for column in target.section.columns {
                if column.link.is_some() {
                    all.push(Self { target, column });
                }
            }
        }
        all
    }
}

pub(crate) fn every_column_count(target: TableTarget, dry_run: bool) -> String {
    let every: Vec<&Column> = target.section.columns.iter().collect();
    section_count(target.section, target.table, &every, dry_run)
}

pub(crate) fn every_column_insert(target: TableTarget, dry_run: bool) -> String {
    let every: Vec<&Column> = target.section.columns.iter().collect();
    section_insert(target.section, target.table, &every, dry_run)
}

pub(crate) fn every_link_insert(link: LinkColumn, dry_run: bool) -> String {
    let LinkColumn { target, column } = link;
    link_insert(target.section, target.table, column, dry_run).unwrap_or_default()
}

pub(crate) fn every_link_count(link: LinkColumn, dry_run: bool) -> String {
    let LinkColumn { target, column } = link;
    link_count(target.section, column, dry_run).unwrap_or_default()
}

pub(crate) fn held_entry_insert_holding(every_column: bool) -> String {
    held_entry_insert(&|_| every_column)
}

pub(crate) fn held_file_insert_holding(every_column: bool) -> String {
    held_file_insert(&|_| every_column)
}
