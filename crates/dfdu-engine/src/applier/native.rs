//! Native cleanup commands of rules.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dfdu_clean::DeleterReq;
use dfdu_scan::Backend;
use dfdu_tree::{NodeRef, RuleIx};

use super::{Applier, Model, NativeRun};

/// How long a native cleanup command may run.
const NATIVE_TIMEOUT: Duration = Duration::from_mins(30);

impl<B: Backend> Applier<B> {
    pub(super) fn run_native(
        &mut self,
        m: &mut Model,
        id: u64,
        node: NodeRef,
        variant: Option<usize>,
        dry_run: bool,
    ) {
        if self.read_only && !dry_run {
            m.status.notice("native command: read-only mode".into());
            return;
        }
        let (Some(rules), Some(ix)) = (m.rules.clone(), m.tree.resolve_visible(node)) else {
            return;
        };
        let Some(class) = m.tree.class(ix) else {
            return;
        };
        let path = m.tree.path_buf(ix);
        let Some(req) = variant
            .map_or(Some(None), |v| u16::try_from(v).ok().map(Some))
            .and_then(|v| native_req(&rules, class.rule, v, &path, dry_run, id))
        else {
            return;
        };
        m.status.native = Some(NativeRun {
            id,
            rule: class.rule,
            command: dfdu_clean::shown_argv(&req.command),
            cwd: req.cwd.clone(),
            dry_run,
            lines: VecDeque::new(),
            done: None,
        });
        self.rescan_paths.clear();
        if !dry_run {
            self.rescan_paths.push(path);
        }
        self.outgoing_controls.push(DeleterReq::Native(req));
    }
}

/// A rule's native command as it runs for the matched node at `path`.
#[must_use]
pub fn native_argv(argv: &[String], path: &Path) -> Vec<String> {
    argv.iter().map(|a| native_var(a, path)).collect()
}

/// The native command of `rule` (its `variant`, if given; the dry run with `dry_run`) for the
/// matched node at `path`, or `None` when the rule has no such command. The agent builds what
/// it is asked to run with this from its own rules, as the local side did.
pub(crate) fn native_req(
    rules: &dfdu_rules::RuleSet,
    rule: RuleIx,
    variant: Option<u16>,
    path: &Path,
    dry_run: bool,
    id: u64,
) -> Option<dfdu_clean::NativeReq> {
    let def = rules.rules().get(usize::from(rule.0))?.native.as_ref()?;
    let (command, dry) = match variant {
        None => (&def.command, &def.dry_run),
        Some(v) => {
            let x = def.variants.get(v as usize)?;
            (&x.command, &x.dry_run)
        }
    };
    let argv = if dry_run { dry } else { command };
    if argv.is_empty() {
        return None;
    }
    Some(dfdu_clean::NativeReq {
        id,
        check: def.check.clone(),
        pre: def.pre.clone(),
        command: native_argv(argv, path),
        cwd: def
            .cwd
            .as_deref()
            .map(|c| PathBuf::from(native_var(c, path))),
        timeout: NATIVE_TIMEOUT,
        dry_run,
        rule: Some(dfdu_clean::NativeRule {
            rule,
            variant,
            path: path.to_path_buf(),
        }),
    })
}

/// Expands the variables a native command may use, for the matched node at `path`: `{path}`,
/// `{parent}`, `{name}` and `{name_stem}`.
fn native_var(arg: &str, path: &Path) -> String {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let raw = dfdu_tree::name_bytes(path.file_name().unwrap_or_default());
    let stem = String::from_utf8_lossy(dfdu_rules::stem(&raw));
    arg.replace("{path}", &path.to_string_lossy())
        .replace("{parent}", &path.parent().unwrap_or(path).to_string_lossy())
        .replace("{name_stem}", &stem)
        .replace("{name}", &name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_expand_for_the_matched_node() {
        let argv = [
            "tar",
            "-czf",
            "{name_stem}.tgz",
            "-C",
            "{parent}",
            "{name}",
            "{path}",
        ]
        .map(String::from);
        let got = native_argv(&argv, Path::new("/srv/web/src.tar.gz"));
        assert_eq!(
            got,
            [
                "tar",
                "-czf",
                "src.tgz",
                "-C",
                "/srv/web",
                "src.tar.gz",
                "/srv/web/src.tar.gz"
            ]
        );
    }
}
