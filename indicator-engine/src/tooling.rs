//! The checks behind `orbit-tape-indicator check` and `test`, as a library so
//! other front ends (the Orbit Tape MCP server) report exactly the same thing.

use crate::{
    ChartFrame, CompileLimits, CompiledIndicator, Diagnostic, FrameOutput, IndicatorConfig,
    IndicatorFrame, IndicatorId, InstrumentContext, RuntimeLimits, Severity, TimeOrTick,
    TradeAggregate, compile,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// One source that compiled and passed the ID and filename checks.
pub struct CompiledSource {
    pub path: PathBuf,
    pub source: String,
    pub indicator: CompiledIndicator,
}

/// Compiled sources plus every diagnostic, formatted `path:line:column: error: ...`.
pub struct SourceReport {
    pub sources: Vec<CompiledSource>,
    pub diagnostics: Vec<String>,
}

pub fn load_sources(paths: &[PathBuf], namespace: Option<&str>) -> SourceReport {
    let mut report = SourceReport {
        sources: Vec::new(),
        diagnostics: Vec::new(),
    };
    let mut ids = BTreeMap::<String, PathBuf>::new();
    let limits = CompileLimits::default();

    for path in paths {
        match fs::metadata(path) {
            Ok(metadata) if metadata.len() > limits.max_source_bytes as u64 => {
                report.diagnostics.push(format!(
                    "{}: source exceeds compile limit of {} bytes",
                    path.display(),
                    limits.max_source_bytes
                ));
                continue;
            }
            Err(error) => {
                report.diagnostics.push(format!(
                    "{}: could not read source: {error}",
                    path.display()
                ));
                continue;
            }
            Ok(_) => {}
        }
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                report.diagnostics.push(format!(
                    "{}: could not read source: {error}",
                    path.display()
                ));
                continue;
            }
        };
        let indicator = match compile(&source, path) {
            Ok(indicator) => indicator,
            Err(diagnostics) => {
                report.diagnostics.extend(
                    diagnostics
                        .iter()
                        .map(|diagnostic| format_diagnostic(diagnostic, &source)),
                );
                continue;
            }
        };

        let id = indicator.descriptor.id.as_str().to_owned();
        let mut invalid = false;
        if let Some(namespace) = namespace {
            let has_namespace = id
                .strip_prefix(namespace)
                .is_some_and(|suffix| suffix.starts_with('.'));
            if !has_namespace {
                report.diagnostics.push(format!(
                    "{}:1:1: error: indicator ID '{}' must use namespace '{}'",
                    path.display(),
                    id,
                    namespace
                ));
                invalid = true;
            }
        }
        if let Err(diagnostic) = validate_source_filename(path, &indicator.descriptor.id) {
            report.diagnostics.push(diagnostic);
            invalid = true;
        }
        if let Some(previous) = ids.get(&id) {
            report.diagnostics.push(format!(
                "{}:1:1: error: duplicate indicator ID '{}' (already defined in {})",
                path.display(),
                id,
                previous.display()
            ));
            invalid = true;
        } else {
            ids.insert(id, path.clone());
        }
        if invalid {
            continue;
        }
        report.sources.push(CompiledSource {
            path: path.clone(),
            source,
            indicator,
        });
    }

    report
}

pub fn validate_source_filename(path: &Path, id: &IndicatorId) -> Result<(), String> {
    let expected_file_name = id.source_file_name();
    let actual_file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<non-UTF-8 filename>");
    if actual_file_name == expected_file_name {
        return Ok(());
    }

    Err(format!(
        "{}:1:1: error: source filename '{}' must be '{}' for indicator ID '{}'; rename the file before installing",
        path.display(),
        actual_file_name,
        expected_file_name,
        id.as_str()
    ))
}

pub fn smoke_test(source: &CompiledSource) -> Result<(), String> {
    let config = IndicatorConfig::default();
    let limits = RuntimeLimits::untrusted();
    let warmup = source
        .indicator
        .warmup_bars_with_limits(&config, limits)
        .unwrap_or(source.indicator.descriptor.max_bars_back);
    let frame_count = warmup.saturating_add(2).max(32);
    let mut instance = source
        .indicator
        .instantiate_with_limits(config, limits)
        .map_err(|error| format!("could not instantiate indicator: {error}"))?;

    for index in 0..frame_count {
        let frame = smoke_frame(index);
        let output = instance
            .evaluate_closed(frame.clone())
            .map_err(|error| format!("closed frame {index} failed: {error}"))?;
        validate_output(&source.indicator, &frame, &output)?;
    }

    let frame = smoke_frame(frame_count);
    let output = instance
        .preview_open(frame.clone())
        .map_err(|error| format!("open preview at frame {frame_count} failed: {error}"))?;
    validate_output(&source.indicator, &frame, &output)
}

/// Synthetic bar `index` used by [`smoke_test`].
pub fn smoke_frame(index: usize) -> IndicatorFrame {
    let step = index as f64;
    let close = 100.0 + step * 0.25 + (index % 7) as f64;
    IndicatorFrame {
        index,
        time: (index as i64).saturating_mul(60_000),
        basis: TimeOrTick::Time,
        instrument: InstrumentContext {
            min_tick: Some(0.1),
        },
        chart: ChartFrame {
            open: Some(close - 0.5),
            high: Some(close + 1.0),
            low: Some(close - 1.0),
            close: Some(close),
            volume: Some(1_000.0 + step),
            directional_volume_available: true,
            buy_qty: 40.0 + (index % 11) as f64,
            sell_qty: 30.0 + (index % 9) as f64,
            trade: Some(TradeAggregate {
                buy_qty: 12.0 + (index % 5) as f64,
                sell_qty: 8.0 + (index % 4) as f64,
                buy_count: 3 + (index % 6) as u64,
                sell_count: 2 + (index % 5) as u64,
            }),
        },
        open_interest: Some(10_000.0 + step * 2.0),
        series_inputs: Vec::new().into(),
    }
}

fn validate_output(
    indicator: &CompiledIndicator,
    frame: &IndicatorFrame,
    output: &FrameOutput,
) -> Result<(), String> {
    if output.index != frame.index || output.time != frame.time {
        return Err(format!(
            "output identity mismatch: expected index {} at {}, got index {} at {}",
            frame.index, frame.time, output.index, output.time
        ));
    }

    let output_ids = output
        .plots
        .iter()
        .map(|plot| plot.id.as_str())
        .collect::<BTreeSet<_>>();
    for plot in &indicator.descriptor.plots {
        if !output_ids.contains(plot.id.as_str()) {
            return Err(format!("declared plot '{}' was not emitted", plot.id));
        }
    }
    Ok(())
}

pub fn source_paths(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_owned()]);
    }

    let (paths, diagnostics) = crate::source_files_in_directory(path)?;
    if !diagnostics.is_empty() {
        return Err(diagnostics.join("\n"));
    }
    if paths.is_empty() {
        return Err(format!(
            "{}: directory contains no .ind sources",
            path.display()
        ));
    }
    Ok(paths)
}

pub fn format_diagnostic(diagnostic: &Diagnostic, source: &str) -> String {
    let (line, column) = line_column(source, diagnostic.span.start);
    format!(
        "{}:{line}:{column}: {}: {}",
        diagnostic.path.display(),
        match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        },
        diagnostic.message
    )
}

fn line_column(source: &str, character_offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for character in source.chars().take(character_offset) {
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}
