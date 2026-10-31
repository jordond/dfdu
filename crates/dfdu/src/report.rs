//! `dfdu report`: the rules' matches in a scan, largest first, with what each is, how safe it
//! is to delete and how. `dfdu clean` deletes a selection of the same list. What it prints is in
//! `output`.

mod output;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Args;
use dfdu_rules::RuleSet;
use dfdu_tree::{Safety, now_secs};
use serde_json::json;

use crate::args::{ReportFormat, safety_arg, size_arg};
use crate::matches::{Filter, Item, item_json, items, reclaimable, scan};
use crate::output::{json as js, outside};
use crate::{EXIT_PARTIAL, MountArgs, launch, print_out, written};
use output::{summary, table};

/// Exit code when `--fail-above` is exceeded.
pub const EXIT_THRESHOLD: u8 = 4;

#[derive(Args)]
pub struct ReportArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
    #[command(flatten)]
    pub filter: Filter,
    /// Only matches this safe or safer: safe, regenerable, review, manual or protected (all).
    #[arg(long, value_name = "LEVEL", value_parser = safety_arg)]
    pub safety: Option<Safety>,
    /// Exit with code 4 when the matches listed, protected ones left out, add up to more than
    /// this (`5GiB`), for CI.
    #[arg(long, value_name = "SIZE", value_parser = size_arg)]
    pub fail_above: Option<u64>,
    /// Output: table or json (schema/report.v1.json).
    #[arg(long, value_enum, default_value_t = ReportFormat::Table)]
    pub format: ReportFormat,
    /// Worker threads.
    #[arg(long)]
    pub threads: Option<usize>,
    #[command(flatten)]
    pub mounts: MountArgs,
}

pub fn run(args: &ReportArgs, rules: &Arc<RuleSet>) -> Result<ExitCode, ExitCode> {
    let (engine, elapsed) = scan(&args.path, args.mounts, args.threads, rules.clone(), false)?;
    let now = now_secs();
    let code = engine.read(|m| {
        let Some(root) = m.tree.roots().next() else {
            return Err(ExitCode::from(1));
        };
        let max = args.safety.unwrap_or(Safety::Protected);
        let list: Vec<Item> = items(m, rules, &args.filter, now)
            .into_iter()
            .filter(|i| i.safety() <= max)
            .collect();
        let total = reclaimable(&list);
        let exceeded = args.fail_above.is_some_and(|t| total > t);
        let left_out = outside::gather(m, root, true);
        if args.format == ReportFormat::Json {
            let mut doc = js::document(&m.tree.host_path(root));
            doc.insert("generated_at".into(), now.into());
            doc.insert("complete".into(), m.complete(root).into());
            doc.insert(
                "elapsed_ms".into(),
                u64::try_from(elapsed.as_millis())
                    .unwrap_or(u64::MAX)
                    .into(),
            );
            doc.insert(
                "items".into(),
                list.iter()
                    .map(|i| item_json(rules, i, now))
                    .collect::<Vec<_>>()
                    .into(),
            );
            doc.insert("summary".into(), summary(&list));
            outside::put_json(&mut doc, &left_out, Some(rules));
            if let Some(t) = args.fail_above {
                doc.insert(
                    "threshold".into(),
                    json!({ "fail_above": t, "exceeded": exceeded }),
                );
            }
            written(js::print(doc))?;
        } else {
            let lines = outside::lines(&left_out, Some(rules), "dfdu report");
            print_out(|w| {
                table(w, rules, &list, now, args.fail_above, exceeded)?;
                outside::print(w, &lines)
            })?;
        }
        Ok(if exceeded {
            ExitCode::from(EXIT_THRESHOLD)
        } else if !m.complete(root) {
            ExitCode::from(EXIT_PARTIAL)
        } else {
            ExitCode::SUCCESS
        })
    });
    engine.shutdown(launch::SHUTDOWN);
    code
}
