//! Integration tests for the app itself, auth, providers, plugins and jobs.

#[path = "../common/mod.rs"]
mod common;

mod admin_backups;
mod admin_routes;
mod auth_e2e;
mod auth_federated;
mod auth_ownership;
mod auth_stores;
mod auth_users;
mod boot;
mod caller_shapes;
mod config_secrets;
mod contract;
mod envelope;
mod events_stream;
mod health;
mod http_gate;
mod jobs_events;
mod jobs_loops;
mod jobs_plugin_ticks;
mod jobs_queue;
mod jobs_supervisor;
mod leak;
mod persist_runtime;
mod persist_schema;
mod plugins_examples;
mod plugins_host;
mod plugins_scrobble;
mod providers_audio;
mod providers_catalog;
mod providers_core;
mod providers_events;
mod providers_gate;
mod providers_models;
mod providers_musicbrainz;
mod route_gate;
mod route_walk;
mod settings_readers;
mod settings_sections;
mod settings_store;
mod source_gates;
mod web;
mod wiring_gate;
