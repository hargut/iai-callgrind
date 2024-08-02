use std::io::stderr;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use log::{debug, info, log_enabled, trace, Level};
use tempfile::TempDir;

use super::args::NoCapture;
use super::callgrind::args::Args;
use super::callgrind::flamegraph::{
    BaselineFlamegraphGenerator, Config as FlamegraphConfig, Flamegraph, FlamegraphGenerator,
};
use super::callgrind::summary_parser::SummaryParser;
use super::callgrind::{CallgrindCommand, RegressionConfig};
use super::format::{Header, OutputFormat, VerticalFormat};
use super::meta::Metadata;
use super::summary::{
    BaselineKind, BaselineName, BenchmarkKind, BenchmarkSummary, CallgrindSummary, CostsSummary,
    SummaryOutput,
};
use super::tool::{
    Parser, RunOptions, ToolConfigs, ToolOutputPath, ToolOutputPathKind, ValgrindTool,
};
use super::{Config, ModulePath};
use crate::api::{self, BinaryBenchmark};
use crate::error::Error;
use crate::runner::format::tool_headline;
use crate::util::{copy_directory, make_relative, write_all_to_stderr};

mod defaults {
    pub const SANDBOX_ENABLED: bool = false;
    pub const REGRESSION_FAIL_FAST: bool = false;
}

// TODO: CLEANUP
#[derive(Debug, Clone)]
struct Assistant {
    kind: AssistantKind,
    group_name: Option<String>,
    indices: Option<(usize, usize)>,
}

// TODO: CLEANUP
#[derive(Debug, Clone)]
enum AssistantKind {
    Setup,
    Teardown,
}

#[derive(Debug)]
struct BaselineBenchmark {
    baseline_kind: BaselineKind,
}

#[derive(Debug)]
struct BinBench {
    id: Option<String>,
    args: Option<String>,
    function_name: String,
    command: api::Command,
    run_options: RunOptions,
    callgrind_args: Args,
    flamegraph_config: Option<FlamegraphConfig>,
    regression_config: Option<RegressionConfig>,
    tools: ToolConfigs,
    setup: Option<Assistant>,
    teardown: Option<Assistant>,
    sandbox: Sandbox,
}

#[derive(Debug)]
struct Group {
    name: String,
    module_path: ModulePath,
    // TODO: Use a Sandbox struct
    // fixtures: Option<api::Fixtures>,
    // sandbox: bool,
    benches: Vec<BinBench>,
    setup: Option<Assistant>,
    teardown: Option<Assistant>,
}

#[derive(Debug)]
struct Groups(Vec<Group>);

#[derive(Debug)]
struct LoadBaselineBenchmark {
    loaded_baseline: BaselineName,
    baseline: BaselineName,
}

#[derive(Debug)]
struct Runner {
    groups: Groups,
    config: Config,
    benchmark: Box<dyn Benchmark>,
    setup: Option<Assistant>,
    teardown: Option<Assistant>,
}

#[derive(Debug)]
struct Sandbox {
    enabled: bool,
    fixtures: Vec<PathBuf>,
    current_dir: PathBuf,
    temp_dir: Option<TempDir>,
}

#[derive(Debug)]
struct SaveBaselineBenchmark {
    baseline: BaselineName,
}

trait Benchmark: std::fmt::Debug {
    fn output_path(&self, bin_bench: &BinBench, config: &Config, group: &Group) -> ToolOutputPath;
    fn baselines(&self) -> (Option<String>, Option<String>);
    fn run(&self, bin_bench: &BinBench, config: &Config, group: &Group)
    -> Result<BenchmarkSummary>;
}

// TODO: CHECK THIS. JUST COPIED FROM lib_bench
impl Assistant {
    fn new_main(kind: AssistantKind) -> Self {
        Self {
            kind,
            group_name: None,
            indices: None,
        }
    }

    fn new_group(kind: AssistantKind, group_name: &str) -> Self {
        Self {
            kind,
            group_name: Some(group_name.to_owned()),
            indices: None,
        }
    }

    fn new_bench(kind: AssistantKind, group_name: &str, indices: (usize, usize)) -> Self {
        Self {
            kind,
            group_name: Some(group_name.to_owned()),
            indices: Some(indices),
        }
    }

    /// Run the `Assistant` but don't benchmark it
    fn run(&self, config: &Config, module_path: &ModulePath) -> Result<()> {
        let id = self.kind.id();
        let nocapture = config.meta.args.nocapture;

        let mut command = Command::new(&config.bench_bin);
        command.arg("--iai-run");

        if let Some(group_name) = &self.group_name {
            command.arg(group_name);
        }

        command.arg(&id);

        if let Some((group_index, bench_index)) = &self.indices {
            command.args([group_index.to_string(), bench_index.to_string()]);
        }

        nocapture.apply(&mut command);

        match nocapture {
            NoCapture::False => {
                let output = command
                    .output()
                    .map_err(|error| {
                        Error::LaunchError(config.bench_bin.clone(), error.to_string())
                    })
                    .and_then(|output| {
                        if output.status.success() {
                            Ok(output)
                        } else {
                            let status = output.status;
                            Err(Error::ProcessError((
                                module_path.join(&id).to_string(),
                                Some(output),
                                status,
                                None,
                            )))
                        }
                    })?;

                if log_enabled!(Level::Info) && !output.stdout.is_empty() {
                    info!("{id} function in group '{module_path}': stdout:");
                    write_all_to_stderr(&output.stdout);
                }

                if log_enabled!(Level::Info) && !output.stderr.is_empty() {
                    info!("{id} function in group '{module_path}': stderr:");
                    write_all_to_stderr(&output.stderr);
                }
            }
            NoCapture::True | NoCapture::Stderr | NoCapture::Stdout => {
                command
                    .status()
                    .map_err(|error| {
                        Error::LaunchError(config.bench_bin.clone(), error.to_string())
                    })
                    .and_then(|status| {
                        if status.success() {
                            Ok(())
                        } else {
                            Err(Error::ProcessError((
                                format!("{module_path}::{id}"),
                                None,
                                status,
                                None,
                            )))
                        }
                    })?;
            }
        };

        Ok(())
    }
}

impl AssistantKind {
    fn id(&self) -> String {
        match self {
            AssistantKind::Setup => "setup",
            AssistantKind::Teardown => "teardown",
        }
        .to_owned()
    }
}

impl Benchmark for BaselineBenchmark {
    fn output_path(&self, bin_bench: &BinBench, config: &Config, group: &Group) -> ToolOutputPath {
        ToolOutputPath::new(
            ToolOutputPathKind::Out,
            ValgrindTool::Callgrind,
            &self.baseline_kind,
            &config.meta.target_dir,
            &group.module_path,
            &bin_bench.name(),
        )
    }

    fn baselines(&self) -> (Option<String>, Option<String>) {
        match &self.baseline_kind {
            BaselineKind::Old => (None, None),
            BaselineKind::Name(name) => (None, Some(name.to_string())),
        }
    }

    fn run(
        &self,
        bin_bench: &BinBench,
        config: &Config,
        group: &Group,
    ) -> Result<BenchmarkSummary> {
        let callgrind_command = CallgrindCommand::new(&config.meta);

        let out_path = self.output_path(bin_bench, config, group);
        out_path.init()?;
        out_path.shift()?;

        let old_path = out_path.to_base_path();
        let log_path = out_path.to_log_output();
        log_path.shift()?;

        for path in bin_bench.tools.output_paths(&out_path) {
            path.shift()?;
            path.to_log_output().shift()?;
        }

        let mut benchmark_summary = bin_bench.create_benchmark_summary(config, group, &out_path)?;

        let header = bin_bench.print_header(&config.meta, group);

        let output = callgrind_command.run(
            bin_bench.callgrind_args.clone(),
            // TODO: WHAT IF PATH IS RELATIVE ??
            &bin_bench.command.path,
            &bin_bench.command.args,
            bin_bench.run_options.clone(),
            &out_path,
        )?;

        let new_costs = SummaryParser.parse(&out_path)?;

        #[allow(clippy::if_then_some_else_none)]
        let old_costs = if old_path.exists() {
            Some(SummaryParser.parse(&old_path)?)
        } else {
            None
        };

        let costs_summary = CostsSummary::new(&new_costs, old_costs.as_ref());
        VerticalFormat::default().print(&config.meta, self.baselines(), &costs_summary)?;

        output.dump_log(log::Level::Info);
        log_path.dump_log(log::Level::Info, &mut stderr())?;

        let regressions = bin_bench.check_and_print_regressions(&costs_summary);

        let callgrind_summary = benchmark_summary
            .callgrind_summary
            .insert(CallgrindSummary::new(
                log_path.real_paths()?,
                out_path.real_paths()?,
            ));

        callgrind_summary.add_summary(
            &bin_bench.command.path,
            &bin_bench.command.args,
            &old_path,
            costs_summary,
            regressions,
        );

        if let Some(flamegraph_config) = bin_bench.flamegraph_config.clone() {
            callgrind_summary.flamegraphs = BaselineFlamegraphGenerator {
                baseline_kind: self.baseline_kind.clone(),
            }
            .create(
                &Flamegraph::new(header.to_title(), flamegraph_config),
                &out_path,
                None,
                &config.meta.project_root,
            )?;
        }

        benchmark_summary.tool_summaries = bin_bench.tools.run(
            &config.meta,
            &bin_bench.command.path,
            &bin_bench.command.args,
            &bin_bench.run_options,
            &out_path,
            false,
        )?;

        Ok(benchmark_summary)
    }
}

impl BinBench {
    fn name(&self) -> String {
        if let Some(bench_id) = &self.id {
            format!("{}.{}", self.function_name, bench_id)
        } else {
            self.function_name.clone()
        }
    }

    fn print_header(&self, meta: &Metadata, group: &Group) -> Header {
        let path = make_relative(&meta.project_root, &self.command.path);

        let command_args: Vec<String> = self
            .command
            .args
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        let command_args = shlex::try_join(command_args.iter().map(String::as_str)).unwrap();

        let description = format!(
            "({}) -> {} {}",
            self.args.as_ref().map_or("", String::as_str),
            path.display(),
            command_args
        );

        let header = Header::from_module_path(
            &group.module_path.join(&self.function_name),
            self.id.clone(),
            description,
        );

        if meta.args.output_format == OutputFormat::Default {
            header.print();
            if self.tools.has_tools_enabled() {
                println!("{}", tool_headline(ValgrindTool::Callgrind));
            }
        }

        header
    }

    // TODO: DOUBLE CHECK. Just copied from lib_bench
    fn create_benchmark_summary(
        &self,
        config: &Config,
        group: &Group,
        output_path: &ToolOutputPath,
    ) -> Result<BenchmarkSummary> {
        let summary_output = if let Some(format) = config.meta.args.save_summary {
            let output = SummaryOutput::new(format, &output_path.dir);
            output.init()?;
            Some(output)
        } else {
            None
        };

        Ok(BenchmarkSummary::new(
            BenchmarkKind::LibraryBenchmark,
            config.meta.project_root.clone(),
            config.package_dir.clone(),
            config.bench_file.clone(),
            config.bench_bin.clone(),
            // TODO: THIS SHOULD BE A ModulePath
            &[&group.module_path.to_string(), &self.function_name],
            self.id.clone(),
            self.args.clone(),
            summary_output,
        ))
    }

    // TODO: DOUBLE CHECK. Just copied from lib_bench
    fn check_and_print_regressions(
        &self,
        costs_summary: &CostsSummary,
    ) -> Vec<super::summary::CallgrindRegressionSummary> {
        if let Some(regression_config) = &self.regression_config {
            regression_config.check_and_print(costs_summary)
        } else {
            vec![]
        }
    }
}

impl Group {
    fn run(
        &self,
        benchmark: &dyn Benchmark,
        is_regressed: &mut bool,
        config: &Config,
    ) -> Result<()> {
        // TODO: CLEANUP and implement SANDBOX
        // let sandbox = if self.sandbox {
        //     debug!("Setting up sandbox");
        //     Some(Sandbox::setup(&self.fixtures)?)
        // } else {
        //     debug!(
        //         "Sandbox switched off: Running benchmarks in the current directory: '{}'",
        //         std::env::current_dir().unwrap().display()
        //     );
        //     None
        // };

        for bench in &self.benches {
            let sandbox = &bench.sandbox;
            // let sandbox = if let Some(sandbox) = &bench.sandbox {
            //     #[allow(clippy::if_then_some_else_none)]
            //     if sandbox.enabled.unwrap_or(defaults::SANDBOX_ENABLED) {
            //         debug!("Setting up sandbox");
            //         Some(Sandbox::setup(sandbox)?)
            //     } else {
            //         debug!("Sandbox disabled");
            //         None
            //     }
            // } else {
            //     None
            // };

            if let Some(setup) = &bench.setup {
                setup.run(
                    config,
                    &bench.id.as_ref().map_or_else(
                        || self.module_path.join(&bench.function_name),
                        |id| self.module_path.join(&bench.function_name).join(id),
                    ),
                )?;
            }

            let fail_fast = bench
                .regression_config
                .as_ref()
                .map_or(defaults::REGRESSION_FAIL_FAST, |r| r.fail_fast);
            let summary = benchmark.run(bench, config, self)?;
            summary.print_and_save(&config.meta.args.output_format)?;

            if let Some(teardown) = &bench.teardown {
                teardown.run(
                    config,
                    &bench.id.as_ref().map_or_else(
                        || self.module_path.join(&bench.function_name),
                        |id| self.module_path.join(&bench.function_name).join(id),
                    ),
                )?;
            }

            summary.check_regression(is_regressed, fail_fast)?;

            // if let Some(sandbox) = sandbox {
            //     debug!("Removing sandbox");
            //     sandbox.reset();
            // }
        }

        Ok(())
    }
}

impl Groups {
    fn from_binary_benchmark(
        module: &ModulePath,
        benchmark: BinaryBenchmark,
        meta: &Metadata,
    ) -> Result<Self> {
        // TODO: Mostly copied from lib_bench, DOUBLE_CHECK !!
        let global_config = benchmark.config;
        let meta_callgrind_args = meta.args.callgrind_args.clone().unwrap_or_default();
        let current_dir =
            std::env::current_dir().expect("Detecting current directory should succeed");

        let mut groups = vec![];
        for binary_benchmark_group in benchmark.groups {
            let module_path = module.join(&binary_benchmark_group.id);

            let setup = binary_benchmark_group
                .has_setup
                .then_some(Assistant::new_group(
                    AssistantKind::Setup,
                    &binary_benchmark_group.id,
                ));
            let teardown = binary_benchmark_group
                .has_teardown
                .then_some(Assistant::new_group(
                    AssistantKind::Teardown,
                    &binary_benchmark_group.id,
                ));

            let mut group = Group {
                name: binary_benchmark_group.id,
                module_path,
                benches: vec![],
                setup,
                teardown,
            };

            // TODO: JUST COPIED FROM lib_bench. Check if everything's right
            for (group_index, binary_benchmark_benches) in
                binary_benchmark_group.benches.into_iter().enumerate()
            {
                for (bench_index, binary_benchmark_bench) in
                    binary_benchmark_benches.benches.into_iter().enumerate()
                {
                    let mut config = global_config.clone().update_from_all([
                        binary_benchmark_group.config.as_ref(),
                        binary_benchmark_benches.config.as_ref(),
                        binary_benchmark_bench.config.as_ref(),
                    ]);

                    // TODO: TEST
                    let command = binary_benchmark_bench.command;
                    config.envs.extend(command.envs.iter().cloned());
                    let envs = config.resolve_envs();

                    let callgrind_args =
                        Args::from_raw_args(&[&config.raw_callgrind_args, &meta_callgrind_args])?;
                    let flamegraph_config = config.flamegraph_config.map(Into::into);
                    let bin_bench = BinBench {
                        id: binary_benchmark_bench.id,
                        args: binary_benchmark_bench.args,
                        function_name: binary_benchmark_bench.bench,
                        run_options: RunOptions {
                            env_clear: config
                                .env_clear
                                .unwrap_or_else(|| command.env_clear.unwrap_or(true)),
                            entry_point: None,
                            envs,
                            ..Default::default()
                        },
                        command,
                        callgrind_args,
                        flamegraph_config,
                        regression_config: api::update_option(
                            &config.regression_config,
                            &meta.regression_config,
                        )
                        .map(Into::into),
                        tools: ToolConfigs(config.tools.0.into_iter().map(Into::into).collect()),
                        setup: binary_benchmark_bench
                            .has_setup
                            .then_some(Assistant::new_bench(
                                AssistantKind::Setup,
                                &group.name,
                                (group_index, bench_index),
                            )),
                        teardown: binary_benchmark_bench.has_teardown.then_some(
                            Assistant::new_bench(
                                AssistantKind::Teardown,
                                &group.name,
                                (group_index, bench_index),
                            ),
                        ),
                        sandbox: config.sandbox.map_or_else(
                            || Sandbox::new(defaults::SANDBOX_ENABLED, current_dir.clone()),
                            |s| Sandbox::from_api(s, current_dir.clone()),
                        ),
                    };
                    group.benches.push(bin_bench);
                }
            }

            groups.push(group);
        }
        Ok(Self(groups))
    }

    /// Run all [`Group`] benchmarks
    ///
    /// # Errors
    ///
    /// Return an [`anyhow::Error`] with sources:
    ///
    /// * [`Error::RegressionError`] if a regression occurred.
    fn run(&self, benchmark: &dyn Benchmark, config: &Config) -> Result<()> {
        let mut is_regressed = false;
        for group in &self.0 {
            if let Some(setup) = &group.setup {
                setup.run(config, &group.module_path)?;
            }

            group.run(benchmark, &mut is_regressed, config)?;

            if let Some(teardown) = &group.teardown {
                teardown.run(config, &group.module_path)?;
            }
        }

        if is_regressed {
            Err(Error::RegressionError(false).into())
        } else {
            Ok(())
        }
    }
}

impl Benchmark for LoadBaselineBenchmark {
    fn output_path(&self, bin_bench: &BinBench, config: &Config, group: &Group) -> ToolOutputPath {
        todo!()
    }

    fn baselines(&self) -> (Option<String>, Option<String>) {
        todo!()
    }

    fn run(
        &self,
        bin_bench: &BinBench,
        config: &Config,
        group: &Group,
    ) -> Result<BenchmarkSummary> {
        todo!()
    }
}

impl Runner {
    fn new(binary_benchmark: BinaryBenchmark, config: Config) -> Result<Self> {
        let setup = binary_benchmark
            .has_setup
            .then_some(Assistant::new_main(AssistantKind::Setup));
        let teardown = binary_benchmark
            .has_teardown
            .then_some(Assistant::new_main(AssistantKind::Teardown));

        let groups = Groups::from_binary_benchmark(
            &ModulePath::new(&config.module),
            binary_benchmark,
            &config.meta,
        )?;

        let benchmark: Box<dyn Benchmark> =
            if let Some(baseline_name) = &config.meta.args.save_baseline {
                Box::new(SaveBaselineBenchmark {
                    baseline: baseline_name.clone(),
                })
            } else if let Some(baseline_name) = &config.meta.args.load_baseline {
                Box::new(LoadBaselineBenchmark {
                    loaded_baseline: baseline_name.clone(),
                    baseline: config
                        .meta
                        .args
                        .baseline
                        .as_ref()
                        .expect("A baseline should be present")
                        .clone(),
                })
            } else {
                Box::new(BaselineBenchmark {
                    baseline_kind: config
                        .meta
                        .args
                        .baseline
                        .as_ref()
                        .map_or(BaselineKind::Old, |name| BaselineKind::Name(name.clone())),
                })
            };

        Ok(Self {
            groups,
            config,
            benchmark,
            setup,
            teardown,
        })
    }

    fn run(&self) -> Result<()> {
        // TODO: DON'T RUN ANY SETUP OR TEARDOWN functions if --load-baseline is given
        if let Some(setup) = &self.setup {
            setup.run(&self.config, &ModulePath::new(&self.config.module))?;
        }

        self.groups.run(self.benchmark.as_ref(), &self.config)?;

        if let Some(teardown) = &self.teardown {
            teardown.run(&self.config, &ModulePath::new(&self.config.module))?;
        }
        Ok(())
    }
}

impl Sandbox {
    fn setup(&self) -> Result<()> {
        debug!("Creating temporary workspace directory");
        let temp_dir = tempfile::tempdir().expect("Create temporary directory");

        // if let Some(fixtures) = &fixtures {
        //     debug!(
        //         "Copying fixtures from '{}' to '{}'",
        //         &fixtures.path.display(),
        //         temp_dir.path().display()
        //     );
        //     copy_directory(&fixtures.path, temp_dir.path(), fixtures.follow_symlinks)?;
        // }

        let current_dir = std::env::current_dir()
            .with_context(|| "Failed to detect current directory".to_owned())?;

        trace!(
            "Changing current directory to temporary directory: '{}'",
            temp_dir.path().display()
        );

        let path = temp_dir.path();
        std::env::set_current_dir(path).with_context(|| {
            format!(
                "Failed setting current directory to temporary workspace directory: '{}'",
                path.display()
            )
        })?;

        Ok(())
    }

    fn reset(self) {
        // std::env::set_current_dir(&self.current_dir)
        //     .expect("Reset current directory to package directory");

        // if log_enabled!(Level::Debug) {
        //     debug!("Removing temporary workspace");
        //     if let Err(error) = self.temp_dir.close() {
        //         debug!("Error trying to delete temporary workspace: {error}");
        //     }
        // } else {
        //     _ = self.temp_dir.close();
        // }
    }

    fn new(enabled: bool, current_dir: PathBuf) -> Self {
        Self {
            enabled,
            fixtures: vec![],
            current_dir,
            temp_dir: None,
        }
    }

    fn from_api(s: api::Sandbox, current_dir: PathBuf) -> Sandbox {
        Self {
            enabled: s.enabled.unwrap_or(defaults::SANDBOX_ENABLED),
            fixtures: s.fixtures,
            current_dir,
            temp_dir: None,
        }
    }
}

// TODO: CLEANUP
// impl From<api::Sandbox> for Sandbox {
//     fn from(value: api::Sandbox) -> Self {
//         Self { enabled: value.enabled.unwrap_or(defaults::SANDBOX_ENABLED), fixtures:
// value.fixtures, current_dir: (), temp_dir: () }     }
// }

impl Benchmark for SaveBaselineBenchmark {
    fn output_path(&self, bin_bench: &BinBench, config: &Config, group: &Group) -> ToolOutputPath {
        todo!()
    }

    fn baselines(&self) -> (Option<String>, Option<String>) {
        todo!()
    }

    fn run(
        &self,
        bin_bench: &BinBench,
        config: &Config,
        group: &Group,
    ) -> Result<BenchmarkSummary> {
        todo!()
    }
}

pub fn run(binary_benchmark: BinaryBenchmark, config: Config) -> Result<()> {
    Runner::new(binary_benchmark, config)?.run()
}
