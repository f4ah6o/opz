mod cli;
mod instrumentation;
pub(crate) use cli::*;
mod mcp;
pub(crate) use mcp::*;
mod doctor;
pub(crate) use doctor::*;
mod cloudflare;
pub(crate) use cloudflare::*;
mod sdk_bridge;
pub(crate) use sdk_bridge::*;
mod resolver;
pub(crate) use resolver::*;
mod migration;
pub(crate) use migration::*;
mod op;
pub(crate) use op::*;
mod envfile;
pub(crate) use envfile::*;
mod environment;
pub(crate) use environment::*;
mod targets;
pub(crate) use targets::*;
mod process;
pub(crate) use process::*;
mod plugin;
pub(crate) use plugin::*;
mod security;
pub(crate) use security::*;
mod skill;
pub(crate) use skill::*;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use instrumentation::KeyValue;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};

const GITHUB_REPOSITORIES_LABEL: &str = "github_repositories";

/// Binary entrypoint. This is intentionally not a supported library API.
#[doc(hidden)]
pub fn main_entry() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().collect();
    let command_hint = detect_command_hint(&args).to_string();

    let result = instrumentation::with_span(
        &format!("cli.{command_hint}"),
        vec![KeyValue::new("cli.command", command_hint)],
        || {
            let result = run_cli(&args);
            if let Err(err) = &result {
                if !is_clap_display_error(err) {
                    instrumentation::record_error_message(&err.to_string());
                }
            }
            result
        },
    );

    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            if let Some(clap_err) = err.downcast_ref::<clap::Error>() {
                let _ = clap_err.print();
                std::process::exit(clap_err.exit_code());
            }
            if err.downcast_ref::<DoctorFailure>().is_some() {
                std::process::exit(1);
            }
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests;
