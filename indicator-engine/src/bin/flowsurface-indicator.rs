use flowsurface_indicator_engine::{
    ChartFrame, CompileLimits, CompiledIndicator, Diagnostic, FrameOutput, IndicatorConfig,
    IndicatorFrame, IndicatorId, InputValue, InstrumentContext, RegionOutput, RegionUpdate,
    RuntimeLimits, Severity, TimeOrTick, TradeAggregate, Value, ValueType, compile,
};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;

fn main() {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if let Some(output) = global_output(&arguments) {
        println!("{output}");
        return;
    }
    let result = match arguments.first().map(String::as_str) {
        Some("new") => command_new(&arguments[1..]),
        Some("check") => command_check(&arguments[1..], "check"),
        Some("test") => command_test(&arguments[1..]),
        Some("catalog") => command_catalog(&arguments[1..]),
        Some("run") => command_run(&arguments[1..]),
        Some(command) => Err(format!("unknown subcommand '{command}'")),
        None => Err(usage()),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn usage() -> String {
    "usage: flowsurface-indicator <new|check|test|catalog> ...\n\n  flowsurface-indicator new [directory] --id <namespace.name> --title <Title>\n  flowsurface-indicator check <file.ind|directory> [--namespace <prefix>]\n  flowsurface-indicator test <file.ind|directory> [--namespace <prefix>]\n  flowsurface-indicator catalog <file.ind|directory> --output <catalog.json> [--namespace <prefix>]\n  flowsurface-indicator run <file.ind> --bars <candles.csv> [--out <values.csv>] [--regions <regions.csv>] [--min-tick <tick>] [--input <id>=<value> ...]"
        .to_owned()
}

fn global_output(arguments: &[String]) -> Option<String> {
    match arguments.first().map(String::as_str) {
        Some("--help" | "-h") => Some(usage()),
        Some("--version" | "-V") => Some(env!("CARGO_PKG_VERSION").to_owned()),
        _ => None,
    }
}

fn command_new(arguments: &[String]) -> Result<(), String> {
    if wants_help(arguments) {
        println!(
            "usage: flowsurface-indicator new [directory] --id <namespace.name> --title <Title>"
        );
        return Ok(());
    }
    let options = parse_new_arguments(arguments)?;
    let id = IndicatorId::new(options.id.clone())
        .map_err(|error| format!("invalid indicator ID '{}': {error}", options.id))?;
    if options.title.trim().is_empty() {
        return Err("new requires a non-empty title".to_owned());
    }
    if options.directory.exists() && !options.directory.is_dir() {
        return Err(format!(
            "new destination {} is not a directory",
            options.directory.display()
        ));
    }
    let source_path = options.directory.join(id.source_file_name());
    match fs::symlink_metadata(&source_path) {
        Ok(_) => {
            return Err(format!(
                "refusing to overwrite existing target {}",
                source_path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot inspect scaffold target {}: {error}",
                source_path.display()
            ));
        }
    }
    fs::create_dir_all(&options.directory)
        .map_err(|error| format!("cannot create {}: {error}", options.directory.display()))?;
    let indicator_line = format!(
        "indicator(\"{}\", id = \"{}\")\nformat(precision = \"base_ticker_min_tick\")",
        escape_string(&options.title),
        id.as_str()
    );
    let source = format!(
        "language(1)\n{indicator_line}\nperiod = input.int(14, \"Period\", min = 1, max = 200)\nvalue = ta.sma(chart.close, period)\nplot(value, id = \"value\", title = \"Value\")\n"
    );
    fs::write(&source_path, source)
        .map_err(|error| format!("cannot write scaffold {}: {error}", source_path.display()))?;
    println!("created {}", source_path.display());
    Ok(())
}

fn command_check(arguments: &[String], command: &str) -> Result<(), String> {
    if wants_help(arguments) {
        println!(
            "usage: flowsurface-indicator {command} <file.ind|directory> [--namespace <prefix>]"
        );
        return Ok(());
    }
    let (path, namespace) = parse_source_arguments(arguments, command)?;
    let paths = source_paths(&path)?;
    let report = load_sources(&paths, namespace.as_deref());
    print_diagnostics(&report.diagnostics);
    if !report.diagnostics.is_empty() {
        return Err(failure_summary(command, report.diagnostics.len()));
    }
    println!("{command} passed: {}", source_summary(report.sources.len()));
    Ok(())
}

fn command_test(arguments: &[String]) -> Result<(), String> {
    if wants_help(arguments) {
        println!("usage: flowsurface-indicator test <file.ind|directory> [--namespace <prefix>]");
        return Ok(());
    }
    let (path, namespace) = parse_source_arguments(arguments, "test")?;
    let paths = source_paths(&path)?;
    let report = load_sources(&paths, namespace.as_deref());
    print_diagnostics(&report.diagnostics);

    let mut failures = report.diagnostics.len();
    for source in &report.sources {
        match smoke_test(source) {
            Ok(()) => println!("test passed: {}", source.path.display()),
            Err(error) => {
                eprintln!("{}:1:1: error: {error}", source.path.display());
                failures += 1;
            }
        }
    }

    if failures == 0 {
        println!("test passed: {}", source_summary(report.sources.len()));
        Ok(())
    } else {
        Err(failure_summary("test", failures))
    }
}

const RUN_USAGE: &str = "usage: flowsurface-indicator run <file.ind> --bars <candles.csv> [--out <values.csv>] [--regions <regions.csv>] [--min-tick <tick>] [--input <id>=<value> ...]

Evaluates every bar of a candle CSV (header with time, open, high, low, close
and optionally volume; time in Unix seconds or milliseconds) as a closed bar and
writes one row per bar with each plot's value. --regions writes the regions on
the chart after the last bar, and the info panel is printed to stdout.";

struct RunArguments {
    path: PathBuf,
    bars: PathBuf,
    out: Option<PathBuf>,
    regions: Option<PathBuf>,
    min_tick: Option<f64>,
    inputs: Vec<(String, String)>,
}

fn parse_run_arguments(arguments: &[String]) -> Result<RunArguments, String> {
    let mut path = None;
    let mut bars = None;
    let mut out = None;
    let mut regions = None;
    let mut min_tick = None;
    let mut inputs = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if !argument.starts_with("--") {
            if path.replace(PathBuf::from(argument)).is_some() {
                return Err("run expects exactly one .ind source file".to_owned());
            }
            index += 1;
            continue;
        }
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("run requires a value after {argument}"))?;
        match argument {
            "--bars" => bars = Some(PathBuf::from(value)),
            "--out" => out = Some(PathBuf::from(value)),
            "--regions" => regions = Some(PathBuf::from(value)),
            "--min-tick" => {
                min_tick = Some(
                    value
                        .parse::<f64>()
                        .ok()
                        .filter(|tick| tick.is_finite() && *tick > 0.0)
                        .ok_or_else(|| format!("run: invalid --min-tick '{value}'"))?,
                );
            }
            "--input" => {
                let (id, input) = value
                    .split_once('=')
                    .ok_or_else(|| format!("run: --input expects <id>=<value>, got '{value}'"))?;
                inputs.push((id.trim().to_owned(), input.to_owned()));
            }
            _ => return Err(format!("run received unknown argument '{argument}'")),
        }
        index += 2;
    }
    Ok(RunArguments {
        path: path.ok_or("run expects a .ind source file")?,
        bars: bars.ok_or("run requires --bars <candles.csv>")?,
        out,
        regions,
        min_tick,
        inputs,
    })
}

/// One OHLCV row of a candle CSV.
#[derive(Debug)]
struct CsvBar {
    time: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: Option<f64>,
}

fn read_bars(path: &Path) -> Result<Vec<CsvBar>, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header = lines
        .next()
        .ok_or_else(|| format!("{}: empty candle file", path.display()))?;
    let columns = header
        .split(',')
        .map(|name| name.trim().trim_matches('"').to_ascii_lowercase())
        .collect::<Vec<_>>();
    let column = |name: &str| columns.iter().position(|column| column == name);
    let required = |name: &str| {
        column(name).ok_or_else(|| format!("{}: missing '{name}' column", path.display()))
    };
    let (time, open, high, low, close) = (
        required("time")?,
        required("open")?,
        required("high")?,
        required("low")?,
        required("close")?,
    );
    let volume = column("volume");

    let mut bars = Vec::new();
    for (line_index, line) in lines.enumerate() {
        let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
        let number = |index: usize, name: &str| {
            fields
                .get(index)
                .and_then(|field| field.trim_matches('"').parse::<f64>().ok())
                .filter(|value| value.is_finite())
                .ok_or_else(|| {
                    format!(
                        "{}:{}: invalid {name} value",
                        path.display(),
                        line_index + 2
                    )
                })
        };
        let raw_time = number(time, "time")?;
        // Seconds until the year 5138; anything larger is milliseconds.
        let time = if raw_time.abs() < 1e11 {
            raw_time * 1000.0
        } else {
            raw_time
        } as i64;
        if bars
            .last()
            .is_some_and(|previous: &CsvBar| previous.time >= time)
        {
            return Err(format!(
                "{}:{}: bar times must increase",
                path.display(),
                line_index + 2
            ));
        }
        bars.push(CsvBar {
            time,
            open: number(open, "open")?,
            high: number(high, "high")?,
            low: number(low, "low")?,
            close: number(close, "close")?,
            volume: volume.map(|index| number(index, "volume")).transpose()?,
        });
    }
    Ok(bars)
}

fn parse_input_value(value_type: ValueType, text: &str) -> Option<InputValue> {
    Some(match value_type {
        ValueType::Int => InputValue::Int(text.parse().ok()?),
        ValueType::Time => InputValue::Time(text.parse().ok()?),
        ValueType::Float => InputValue::Float(text.parse().ok()?),
        ValueType::Bool => InputValue::Bool(match text {
            "true" => true,
            "false" => false,
            _ => return None,
        }),
        ValueType::String => InputValue::String(text.to_owned()),
        ValueType::Color => {
            let hex = text.strip_prefix('#')?;
            let rgba = u32::from_str_radix(hex, 16).ok()?;
            InputValue::Color(match hex.len() {
                6 => 0xff00_0000 | rgba,
                8 => rgba.rotate_right(8),
                _ => return None,
            })
        }
        _ => return None,
    })
}

fn csv_value(value: &Value) -> String {
    match value {
        Value::Na => String::new(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::String(value) => csv_text(value),
        Value::Color(value) => format!("#{value:08x}"),
        other => csv_text(&format!("{other:?}")),
    }
}

fn csv_text(text: &str) -> String {
    if text.contains([',', '"', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

fn command_run(arguments: &[String]) -> Result<(), String> {
    if wants_help(arguments) {
        println!("{RUN_USAGE}");
        return Ok(());
    }
    let arguments = parse_run_arguments(arguments)?;
    let report = load_sources(&source_paths(&arguments.path)?, None);
    print_diagnostics(&report.diagnostics);
    let [source] = report.sources.as_slice() else {
        return Err(failure_summary("run", report.diagnostics.len().max(1)));
    };
    let indicator = &source.indicator;

    let mut config = IndicatorConfig::default();
    for (id, text) in &arguments.inputs {
        let descriptor = indicator
            .descriptor
            .inputs
            .iter()
            .find(|input| input.id == *id || input.label == *id)
            .ok_or_else(|| format!("run: unknown input '{id}'"))?;
        let value = parse_input_value(descriptor.value_type, text).ok_or_else(|| {
            format!(
                "run: invalid value '{text}' for input '{}' ({:?})",
                descriptor.id, descriptor.value_type
            )
        })?;
        config = config.with(descriptor.id.clone(), value);
    }
    let mut instance = indicator
        .instantiate_with_limits(config, RuntimeLimits::untrusted())
        .map_err(|error| format!("could not instantiate indicator: {error}"))?;

    let bars = read_bars(&arguments.bars)?;
    let plot_ids = indicator
        .descriptor
        .plots
        .iter()
        .map(|plot| plot.id.clone())
        .collect::<Vec<_>>();
    let mut rows = vec![
        ["index".to_owned(), "time".to_owned()]
            .into_iter()
            .chain(plot_ids.iter().cloned())
            .collect::<Vec<_>>()
            .join(","),
    ];
    let mut persistent = BTreeMap::<(String, i64), RegionOutput>::new();
    let mut last = FrameOutput::default();
    for (index, bar) in bars.iter().enumerate() {
        let frame = IndicatorFrame {
            index,
            time: bar.time,
            basis: TimeOrTick::Time,
            instrument: InstrumentContext {
                min_tick: arguments.min_tick,
            },
            chart: ChartFrame {
                open: Some(bar.open),
                high: Some(bar.high),
                low: Some(bar.low),
                close: Some(bar.close),
                volume: bar.volume,
                directional_volume_available: false,
                buy_qty: 0.0,
                sell_qty: 0.0,
                trade: None,
            },
            open_interest: None,
            ..smoke_frame(index)
        };
        let output = instance
            .evaluate_closed(frame)
            .map_err(|error| format!("bar {index} ({}) failed: {error}", bar.time))?;
        let values = output
            .plots
            .iter()
            .map(|plot| (plot.id.as_str(), &plot.value))
            .collect::<BTreeMap<_, _>>();
        let mut row = vec![index.to_string(), bar.time.to_string()];
        row.extend(plot_ids.iter().map(|id| {
            values
                .get(id.as_str())
                .map_or_else(String::new, |value| csv_value(value))
        }));
        rows.push(row.join(","));
        for update in output.region_updates.iter() {
            match update {
                RegionUpdate::Upsert(region) => {
                    persistent.insert((region.id.clone(), region.key), region.clone());
                }
                RegionUpdate::Remove { id, key } => {
                    persistent.remove(&(id.clone(), *key));
                }
            }
        }
        last = output;
    }

    let values = rows.join("\n") + "\n";
    match &arguments.out {
        Some(path) => {
            fs::write(path, values).map_err(|error| format!("{}: {error}", path.display()))?
        }
        None => print!("{values}"),
    }

    if let Some(path) = &arguments.regions {
        let mut lines =
            vec!["id,key,persistent,left_index,right_index,upper,lower,text".to_owned()];
        let active = last.regions.iter().map(|region| (false, region));
        let kept = persistent.values().map(|region| (true, region));
        for (is_persistent, region) in active.chain(kept) {
            lines.push(
                [
                    csv_text(&region.id),
                    region.key.to_string(),
                    is_persistent.to_string(),
                    region.left_index.to_string(),
                    region.right_index.to_string(),
                    region.upper.to_string(),
                    region.lower.to_string(),
                    region
                        .text
                        .as_ref()
                        .map_or_else(String::new, |text| csv_text(&text.content)),
                ]
                .join(","),
            );
        }
        fs::write(path, lines.join("\n") + "\n")
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }

    if !last.info_rows.is_empty() {
        let mut panels = BTreeMap::<&str, Vec<_>>::new();
        for row in last.info_rows.iter() {
            panels.entry(row.id.as_str()).or_default().push(row);
        }
        for (id, mut rows) in panels {
            rows.sort_by_key(|row| row.row);
            let width = rows
                .iter()
                .map(|row| row.left.chars().count())
                .max()
                .unwrap_or(0);
            eprintln!("info panel '{id}' after the last bar:");
            for row in rows {
                eprintln!("  {:width$}  {}", row.left, row.right);
            }
        }
    }
    eprintln!("run: {} bars evaluated", bars.len());
    Ok(())
}

fn command_catalog(arguments: &[String]) -> Result<(), String> {
    if wants_help(arguments) {
        println!(
            "usage: flowsurface-indicator catalog <file.ind|directory> --output <catalog.json> [--namespace <prefix>]"
        );
        return Ok(());
    }

    let CatalogArguments {
        path,
        namespace,
        output,
    } = parse_catalog_arguments(arguments)?;

    let paths = source_paths(&path)?;
    let mut report = load_sources(&paths, namespace.as_deref());
    print_diagnostics(&report.diagnostics);
    if !report.diagnostics.is_empty() {
        return Err(failure_summary("catalog", report.diagnostics.len()));
    }

    report.sources.sort_by(|left, right| {
        left.indicator
            .descriptor
            .id
            .cmp(&right.indicator.descriptor.id)
    });

    let root = if path.is_dir() {
        path.as_path()
    } else {
        path.parent().unwrap_or_else(|| Path::new(""))
    };
    let entries = report
        .sources
        .iter()
        .map(|source| source.catalog_entry(root))
        .collect::<Result<Vec<_>, _>>()?;
    let catalog = serde_json::json!({
        "schema_version": 1,
        "generator_version": env!("CARGO_PKG_VERSION"),
        "language_version": 1,
        "indicators": entries,
    });
    let mut json = serde_json::to_vec_pretty(&catalog).map_err(|error| error.to_string())?;
    json.push(b'\n');

    write_catalog(&output, &json)?;
    println!(
        "catalog written: {} ({})",
        output.display(),
        source_summary(report.sources.len())
    );
    Ok(())
}

fn write_catalog(output: &Path, json: &[u8]) -> Result<(), String> {
    let name = output
        .file_name()
        .ok_or_else(|| "catalog output must name a file".to_owned())?;
    let temporary =
        output.with_file_name(format!(".{}.{}.tmp", name.to_string_lossy(), process::id()));
    let mut file = fs::File::create_new(&temporary)
        .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;

    let result = file.write_all(json);
    drop(file);

    if let Err(error) = result.and_then(|()| fs::rename(&temporary, output)) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot write {}: {error}", output.display()));
    }

    Ok(())
}

fn validate_source_filename(path: &Path, id: &IndicatorId) -> Result<(), String> {
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

struct CompiledSource {
    path: PathBuf,
    source: String,
    indicator: CompiledIndicator,
}

impl CompiledSource {
    fn catalog_entry(&self, root: &Path) -> Result<serde_json::Value, String> {
        let relative = self
            .path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?;
        let path = relative
            .components()
            .map(|component| {
                component.as_os_str().to_str().ok_or_else(|| {
                    format!(
                        "{}: catalog source paths must be UTF-8",
                        self.path.display()
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("/");

        let digest = ring::digest::digest(&ring::digest::SHA256, self.source.as_bytes());
        let sha256 = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();

        let descriptor = &self.indicator.descriptor;
        let author = descriptor
            .author
            .as_ref()
            .map(|author| serde_json::json!({ "name": author.name, "url": author.url }));
        let bases = descriptor
            .bases
            .as_deref()
            .unwrap_or(&[TimeOrTick::Time, TimeOrTick::Tick])
            .iter()
            .map(|basis| match basis {
                TimeOrTick::Time => "time",
                TimeOrTick::Tick => "tick",
            })
            .collect::<Vec<_>>();

        Ok(serde_json::json!({
            "id": descriptor.id.as_str(),
            "name": descriptor.title,
            "description": descriptor.description,
            "author": author,
            "source_url": descriptor.source,
            "path": path,
            "sha256": sha256,
            "size_bytes": self.source.len(),
            "bases": bases,
        }))
    }
}

struct SourceReport {
    sources: Vec<CompiledSource>,
    diagnostics: Vec<String>,
}

fn load_sources(paths: &[PathBuf], namespace: Option<&str>) -> SourceReport {
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

fn print_diagnostics(diagnostics: &[String]) {
    for diagnostic in diagnostics {
        eprintln!("{diagnostic}");
    }
}

fn failure_summary(command: &str, failures: usize) -> String {
    format!(
        "{command} failed: {failures} diagnostic{}",
        if failures == 1 { "" } else { "s" }
    )
}

fn source_summary(count: usize) -> String {
    format!("{count} source{}", if count == 1 { "" } else { "s" })
}

fn smoke_test(source: &CompiledSource) -> Result<(), String> {
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

fn smoke_frame(index: usize) -> IndicatorFrame {
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

struct NewArguments {
    directory: PathBuf,
    id: String,
    title: String,
}

fn parse_new_arguments(arguments: &[String]) -> Result<NewArguments, String> {
    let mut directory = PathBuf::from(".");
    let mut id = None;
    let mut title = None;
    let mut index = 0;
    if arguments
        .first()
        .is_some_and(|argument| !argument.starts_with('-'))
    {
        directory = PathBuf::from(&arguments[0]);
        index = 1;
    }
    while index < arguments.len() {
        let option = arguments[index].as_str();
        match option {
            "--id" | "--title" => {
                let Some(value) = arguments
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                else {
                    return Err(format!("new requires {option} <value>"));
                };
                let target = if option == "--id" {
                    &mut id
                } else {
                    &mut title
                };
                if target.replace(value.clone()).is_some() {
                    return Err(format!("new received duplicate {option}"));
                }
                index += 2;
            }
            _ => return Err(format!("new received unknown argument '{option}'")),
        }
    }
    Ok(NewArguments {
        directory,
        id: id.ok_or_else(|| "new requires --id <namespace.name>".to_owned())?,
        title: title.ok_or_else(|| "new requires --title <Title>".to_owned())?,
    })
}

struct CatalogArguments {
    path: PathBuf,
    namespace: Option<String>,
    output: PathBuf,
}

fn parse_catalog_arguments(arguments: &[String]) -> Result<CatalogArguments, String> {
    let mut source_arguments = Vec::new();
    let mut output = None;
    let mut index = 0;

    while index < arguments.len() {
        if arguments[index] == "--output" {
            let value = arguments
                .get(index + 1)
                .filter(|value| !value.starts_with('-'))
                .ok_or_else(|| "catalog requires --output <catalog.json>".to_owned())?;
            if output.replace(PathBuf::from(value)).is_some() {
                return Err("catalog received duplicate --output".to_owned());
            }
            index += 2;
        } else {
            source_arguments.push(arguments[index].clone());
            index += 1;
        }
    }

    let output = output.ok_or_else(|| "catalog requires --output <catalog.json>".to_owned())?;
    if output.extension().and_then(|extension| extension.to_str()) == Some("ind") {
        return Err("catalog output must not be an .ind source file".to_owned());
    }

    let (path, namespace) = parse_source_arguments(&source_arguments, "catalog")?;
    Ok(CatalogArguments {
        path,
        namespace,
        output,
    })
}

fn parse_source_arguments(
    arguments: &[String],
    command: &str,
) -> Result<(PathBuf, Option<String>), String> {
    let mut path = None;
    let mut namespace = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--namespace" => {
                let Some(value) = arguments
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                else {
                    return Err(format!("{command} requires --namespace <prefix>"));
                };
                if namespace.replace(value.clone()).is_some() {
                    return Err(format!("{command} received duplicate --namespace"));
                }
                validate_namespace_prefix(value)?;
                index += 2;
            }
            argument if argument.starts_with('-') => {
                return Err(format!("{command} received unknown argument '{argument}'"));
            }
            _ => {
                if path.replace(PathBuf::from(&arguments[index])).is_some() {
                    return Err(format!(
                        "{command} expects exactly one .ind source file or directory"
                    ));
                }
                index += 1;
            }
        }
    }
    let path =
        path.ok_or_else(|| format!("{command} expects exactly one .ind source file or directory"))?;
    if !path.exists() {
        return Err(format!("{}: source path does not exist", path.display()));
    }
    if path.is_file() && path.extension().and_then(|extension| extension.to_str()) != Some("ind") {
        return Err(format!(
            "{}: indicator source files must use the .ind extension",
            path.display()
        ));
    }
    if !path.is_file() && !path.is_dir() {
        return Err(format!(
            "{}: source path is not a file or directory",
            path.display()
        ));
    }
    Ok((path, namespace))
}

fn validate_namespace_prefix(namespace: &str) -> Result<(), String> {
    IndicatorId::new(format!("{namespace}.indicator"))
        .map(|_| ())
        .map_err(|error| format!("invalid namespace prefix '{namespace}': {error}"))
}

fn source_paths(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_owned()]);
    }

    let (paths, diagnostics) = flowsurface_indicator_engine::source_files_in_directory(path)?;
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

fn format_diagnostic(diagnostic: &Diagnostic, source: &str) -> String {
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

fn wants_help(arguments: &[String]) -> bool {
    arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
}

fn escape_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => escaped.push(' '),
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {

    #[test]
    fn run_reads_candles_in_seconds_or_milliseconds() {
        let directory = std::env::temp_dir().join(format!("run-bars-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("bars.csv");
        fs::write(
            &path,
            "Time,Open,High,Low,Close\n1700000000,1,2,0.5,1.5\n1700003600,1.5,3,1,2\n",
        )
        .unwrap();
        let bars = read_bars(&path).unwrap();
        assert_eq!(
            bars.iter().map(|bar| bar.time).collect::<Vec<_>>(),
            vec![1_700_000_000_000, 1_700_003_600_000]
        );
        assert_eq!(bars[1].high, 3.0);
        assert!(bars[0].volume.is_none());

        fs::write(
            &path,
            "time,open,high,low,close,volume\n1700000000000,1,2,0.5,1.5,7\n",
        )
        .unwrap();
        let bars = read_bars(&path).unwrap();
        assert_eq!(
            (bars[0].time, bars[0].volume),
            (1_700_000_000_000, Some(7.0))
        );

        fs::write(
            &path,
            "time,open,high,low,close\n120,1,2,0.5,1.5\n60,1,2,0.5,1.5\n",
        )
        .unwrap();
        assert!(read_bars(&path).unwrap_err().contains("must increase"));
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn run_parses_input_values_by_type() {
        assert_eq!(
            parse_input_value(ValueType::Color, "#89b4fa"),
            Some(InputValue::Color(0xff89_b4fa))
        );
        assert_eq!(
            parse_input_value(ValueType::Color, "#89b4fa80"),
            Some(InputValue::Color(0x8089_b4fa))
        );
        assert_eq!(
            parse_input_value(ValueType::Bool, "false"),
            Some(InputValue::Bool(false))
        );
        assert_eq!(parse_input_value(ValueType::Int, "1.5"), None);
        assert_eq!(csv_value(&Value::String("a,b".to_owned())), "\"a,b\"");
        assert_eq!(csv_value(&Value::Na), "");
    }
    use super::*;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn cli_commands_validate_sources_and_create_without_overwriting() {
        let parsed = parse_new_arguments(&arguments(&[
            "--id",
            "community.example",
            "--title",
            "Example",
        ]))
        .expect("new arguments should parse");
        assert_eq!(parsed.directory, PathBuf::from("."));
        assert_eq!(parsed.id, "community.example");

        let root = std::env::temp_dir().join(format!(
            "flowsurface-indicator-cli-new-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let directory = root.join("collection");
        command_new(&arguments(&[
            directory.to_str().expect("temporary path should be UTF-8"),
            "--id",
            "community.example",
            "--title",
            "Example",
        ]))
        .expect("new should create a source");

        assert!(directory.join("example.ind").is_file());
        assert!(!directory.join("community.example.ind").exists());
        let overwrite = command_new(&arguments(&[
            directory.to_str().expect("temporary path should be UTF-8"),
            "--id",
            "community.example",
            "--title",
            "Example",
        ]))
        .expect_err("new should not overwrite an existing source");
        assert!(overwrite.contains("refusing to overwrite"));

        command_check(&[directory.to_string_lossy().into_owned()], "check")
            .expect("directory should validate");

        let mismatch = root.join("mismatch");
        fs::create_dir_all(&mismatch).expect("mismatch directory should exist");
        let path = mismatch.join("community.example.ind");
        fs::write(
            &path,
            "language(1)\nindicator(\"Example\", id = \"community.example\")\ndisplay(view = \"plot\")\nformat(precision = \"base_ticker_min_tick\")\nplot(chart.close, id = \"value\")\n",
        )
        .expect("source should exist");
        let check_error = command_check(&[path.to_string_lossy().into_owned()], "check")
            .expect_err("check should reject a nonmatching stem");
        assert!(check_error.contains("1 diagnostic"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn directory_validation_reports_invalid_sources_and_duplicates() {
        let root = std::env::temp_dir().join(format!(
            "flowsurface-indicator-cli-directory-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temporary directory should exist");
        fs::write(
            root.join("first.ind"),
            "language(1)\nindicator(\"First\", id = \"test.first\")\nformat(precision = \"base_ticker_min_tick\")\nplot(chart.close, id = \"value\")\n",
        )
        .expect("first source should be written");
        fs::write(
            root.join("second.ind"),
            "language(1)\nindicator(\"Second\", id = \"test.second\")\nformat(precision = \"base_ticker_min_tick\")\nplot(chart.close, id = \"value\")\n",
        )
        .expect("second source should be written");

        fs::write(root.join("broken.ind"), "language(1)\nindicator(\n")
            .expect("broken source should be written");

        let error = command_check(&[root.to_string_lossy().into_owned()], "check")
            .expect_err("invalid sources should fail");
        assert!(error.contains("1 diagnostic"));

        let type_path = root.join("type-error.ind");
        fs::write(
            &type_path,
            "language(1)\nindicator(\"Type error\", id = \"test.type-error\")\nformat(precision = \"base_ticker_min_tick\")\nvalue = chart.close + true\nplot(value, id = \"value\")\n",
        )
        .expect("type-error source should be written");
        let report = load_sources(&[type_path], None);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("invalid binary operands"))
        );

        let empty = root.join("empty");
        fs::create_dir_all(&empty).expect("empty directory should exist");
        let error = command_check(&[empty.to_string_lossy().into_owned()], "check")
            .expect_err("empty directories should fail");
        assert!(error.contains("contains no .ind sources"));

        let first = root.join("one");
        let second = root.join("two");
        fs::create_dir_all(&first).expect("first duplicate directory should exist");
        fs::create_dir_all(&second).expect("second duplicate directory should exist");
        let first_path = first.join("same.ind");
        let second_path = second.join("same.ind");
        let source = "language(1)\nindicator(\"Same\", id = \"test.same\")\nformat(precision = \"base_ticker_min_tick\")\nplot(chart.close, id = \"value\")\n";
        fs::write(&first_path, source).expect("first duplicate source should be written");
        fs::write(&second_path, source).expect("second duplicate source should be written");
        let report = load_sources(&[first_path, second_path], None);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("duplicate indicator ID"))
        );

        let short_first = root.join("short-one");
        let short_second = root.join("short-two");
        fs::create_dir_all(&short_first).expect("first short-name directory should exist");
        fs::create_dir_all(&short_second).expect("second short-name directory should exist");
        let short_first_path = short_first.join("one.ind");
        let short_second_path = short_second.join("one.ind");
        fs::write(
            &short_first_path,
            "language(1)\nindicator(\"One\", id = \"test.one\")\nformat(precision = \"base_ticker_min_tick\")\nplot(chart.close, id = \"value\")\n",
        )
        .expect("first short-name source should be written");
        fs::write(
            &short_second_path,
            "language(1)\nindicator(\"Two\", id = \"other.one\")\nformat(precision = \"base_ticker_min_tick\")\nplot(chart.close, id = \"value\")\n",
        )
        .expect("second short-name source should be written");
        let paths = source_paths(&root).expect("nested sources should be discovered");
        assert!(paths.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(paths.contains(&short_first_path));
        assert!(paths.contains(&short_second_path));
        let report = load_sources(&[short_first_path, short_second_path], None);
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        assert_eq!(report.sources.len(), 2);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&root, root.join("cycle")).unwrap();
            assert_eq!(source_paths(&root).unwrap(), paths);
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_runs_ta_smoke_and_reports_runtime_failures() {
        let root = std::env::temp_dir().join(format!(
            "flowsurface-indicator-cli-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temporary directory should exist");

        fs::write(
            root.join("smoke.ind"),
            "language(1)\nindicator(\"Smoke\", id = \"test.smoke\")\nformat(precision = \"base_ticker_min_tick\")\nlength = input.int(2, \"Length\", min = 1, max = 10)\nplot(ta.sma(chart.close, length), id = \"value\")\n",
        )
        .expect("smoke source should be written");
        command_test(&[root.to_string_lossy().into_owned()]).expect("TA smoke source should pass");

        fs::write(
            root.join("smoke.ind"),
            "language(1)\nindicator(\"Smoke\", id = \"test.smoke\")\nformat(precision = \"base_ticker_min_tick\")\nwhile true {}\nplot(chart.close, id = \"value\")\n",
        )
        .expect("runtime failure source should be written");
        let error = command_test(&[root.to_string_lossy().into_owned()])
            .expect_err("runtime failure should fail");
        assert!(error.contains("diagnostic"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn namespace_filter_accepts_matching_ids_and_rejects_other_ids() {
        let root = std::env::temp_dir().join(format!(
            "flowsurface-indicator-cli-namespace-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temporary directory should exist");
        let path = root.join("valid.ind");
        fs::write(
            &path,
            "language(1)\nindicator(\"Valid\", id = \"catalog.valid\")\nformat(precision = \"base_ticker_min_tick\")\nplot(chart.close, id = \"value\")\n",
        )
        .expect("source should be written");
        command_check(
            &arguments(&[
                path.to_str().expect("temporary path should be UTF-8"),
                "--namespace",
                "catalog",
            ]),
            "check",
        )
        .expect("matching namespace should pass");

        fs::write(
            &path,
            "language(1)\nindicator(\"Valid\", id = \"core.valid\")\nformat(precision = \"base_ticker_min_tick\")\nplot(chart.close, id = \"value\")\n",
        )
        .expect("source should be rewritten");
        let report = load_sources(&[path], Some("catalog"));
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("must use namespace 'catalog'"))
        );

        let _ = fs::remove_dir_all(root);
    }
}
