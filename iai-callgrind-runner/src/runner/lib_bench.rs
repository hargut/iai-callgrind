//! The `lib_bench` module
//!
//! This module runs all the library benchmarks
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::stderr;

use anyhow::Result;

use super::callgrind::args::Args;
use super::callgrind::flamegraph::{
    BaselineFlamegraphGenerator, Config as FlamegraphConfig, Flamegraph, FlamegraphGenerator,
    LoadBaselineFlamegraphGenerator, SaveBaselineFlamegraphGenerator,
};
use super::callgrind::parser::Sentinel;
use super::callgrind::sentinel_parser::SentinelParser;
use super::callgrind::RegressionConfig;
use super::common::{Assistant, AssistantKind, Config, ModulePath};
use super::format::{
    print_no_capture_footer, LibraryBenchmarkHeader, OutputFormat, VerticalFormat,
};
use super::meta::Metadata;
use super::summary::{
    BaselineKind, BaselineName, BenchmarkKind, BenchmarkSummary, CallgrindRegressionSummary,
    CallgrindSummary, CostsSummary, SummaryOutput,
};
use super::tool::{
    Parser, RunOptions, ToolCommand, ToolConfig, ToolConfigs, ToolOutputPath, ToolOutputPathKind,
    ValgrindTool,
};
use super::{Error, DEFAULT_TOGGLE};
use crate::api::{self, LibraryBenchmarkGroups};

mod defaults {
    pub const COMPARE_BY_ID: bool = false;
}

/// Implements [`Benchmark`] to run a [`LibBench`] and compare against a earlier [`BenchmarkKind`]
#[derive(Debug)]
struct BaselineBenchmark {
    baseline_kind: BaselineKind,
}

// A `Group` is the organizational unit and counterpart of the `library_benchmark_group!` macro
#[derive(Debug)]
struct Group {
    id: String,
    benches: Vec<LibBench>,
    compare_by_id: bool,
    module_path: ModulePath,
    setup: Option<Assistant>,
    teardown: Option<Assistant>,
}

/// `Groups` is the top-level organizational unit of the `main!` macro for library benchmarks
#[derive(Debug)]
struct Groups(Vec<Group>);

/// A `LibBench` represents a single benchmark under the `#[library_benchmark]` attribute macro
///
/// It needs an implementation of `Benchmark` to be run.
#[derive(Debug)]
pub struct LibBench {
    pub bench_index: usize,
    pub index: usize,
    pub id: Option<String>,
    pub function: String,
    pub args: Option<String>,
    pub run_options: RunOptions,
    pub callgrind_args: Args,
    pub flamegraph_config: Option<FlamegraphConfig>,
    pub regression_config: Option<RegressionConfig>,
    pub tools: ToolConfigs,
    pub module_path: ModulePath,
    pub entry_point: Option<String>,
    pub truncate_description: Option<usize>,
}

/// Implements [`Benchmark`] to load a [`LibBench`] baseline run and compare against another
/// baseline
///
/// This benchmark runner does not run valgrind or execute anything.
#[derive(Debug)]
struct LoadBaselineBenchmark {
    loaded_baseline: BaselineName,
    baseline: BaselineName,
}

/// Create and run [`Groups`] with an implementation of [`Benchmark`]
#[derive(Debug)]
struct Runner {
    config: Config,
    groups: Groups,
    benchmark: Box<dyn Benchmark>,
    setup: Option<Assistant>,
    teardown: Option<Assistant>,
}

/// Implements [`Benchmark`] to save a [`LibBench`] run as baseline. If present compare against a
/// former baseline with the same name
#[derive(Debug)]
struct SaveBaselineBenchmark {
    baseline: BaselineName,
}

/// This trait needs to be implemented to actually run a [`LibBench`]
///
/// Despite having the same name, this trait differs from `bin_bench::Benchmark` and is
/// designed to run a `LibBench` only.
trait Benchmark: std::fmt::Debug {
    fn output_path(&self, lib_bench: &LibBench, config: &Config, group: &Group) -> ToolOutputPath;
    fn baselines(&self) -> (Option<String>, Option<String>);
    fn run(&self, lib_bench: &LibBench, config: &Config, group: &Group)
    -> Result<BenchmarkSummary>;
}

impl Benchmark for BaselineBenchmark {
    fn output_path(&self, lib_bench: &LibBench, config: &Config, group: &Group) -> ToolOutputPath {
        ToolOutputPath::new(
            ToolOutputPathKind::Out,
            ValgrindTool::Callgrind,
            &self.baseline_kind,
            &config.meta.target_dir,
            &group.module_path,
            &lib_bench.name(),
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
        lib_bench: &LibBench,
        config: &Config,
        group: &Group,
    ) -> Result<BenchmarkSummary> {
        let header = LibraryBenchmarkHeader::new(&config.meta, lib_bench);
        header.print();

        let callgrind_command = ToolCommand::new(
            ValgrindTool::Callgrind,
            &config.meta,
            config.meta.args.nocapture,
        );

        let mut callgrind_args = lib_bench.callgrind_args.clone();
        if let Some(entry_point) = lib_bench.entry_point.as_ref() {
            callgrind_args.insert_toggle_collect(entry_point);
        }

        let tool_config = ToolConfig::new(ValgrindTool::Callgrind, true, callgrind_args, None);

        let bench_args = lib_bench.bench_args(group);

        let sentinel = Sentinel::default();
        let out_path = self.output_path(lib_bench, config, group);
        out_path.init()?;
        out_path.shift()?;

        let old_path = out_path.to_base_path();
        let log_path = out_path.to_log_output();
        log_path.shift()?;

        for path in lib_bench.tools.output_paths(&out_path) {
            path.shift()?;
            path.to_log_output().shift()?;
        }

        let mut benchmark_summary =
            lib_bench.create_benchmark_summary(config, &out_path, header.description())?;

        let output = callgrind_command.run(
            tool_config,
            &config.bench_bin,
            &bench_args,
            lib_bench.run_options.clone(),
            &out_path,
            &lib_bench.module_path,
            None,
        )?;

        print_no_capture_footer(
            config.meta.args.nocapture,
            lib_bench.run_options.stdout.as_ref(),
            lib_bench.run_options.stderr.as_ref(),
        );

        let new_costs = SentinelParser::new(&sentinel).parse(&out_path)?;

        let old_costs = old_path
            .exists()
            .then(|| SentinelParser::new(&sentinel).parse(&old_path))
            .transpose()?;

        let costs_summary = CostsSummary::new(&new_costs, old_costs.as_ref());
        VerticalFormat::default().print(&config.meta, self.baselines(), &costs_summary)?;

        output.dump_log(log::Level::Info);
        log_path.dump_log(log::Level::Info, &mut stderr())?;

        let regressions = lib_bench.check_and_print_regressions(&costs_summary);

        let callgrind_summary = benchmark_summary
            .callgrind_summary
            .insert(CallgrindSummary::new(
                log_path.real_paths()?,
                out_path.real_paths()?,
            ));

        callgrind_summary.add_summary(
            &config.bench_bin,
            &bench_args,
            &old_path,
            costs_summary,
            regressions,
        );

        if let Some(flamegraph_config) = lib_bench.flamegraph_config.clone() {
            callgrind_summary.flamegraphs = BaselineFlamegraphGenerator {
                baseline_kind: self.baseline_kind.clone(),
            }
            .create(
                &Flamegraph::new(header.to_title(), flamegraph_config),
                &out_path,
                Some(&sentinel),
                &config.meta.project_root,
            )?;
        }

        benchmark_summary.tool_summaries = lib_bench.tools.run(
            config,
            &config.bench_bin,
            &bench_args,
            &lib_bench.run_options,
            &out_path,
            false,
            &lib_bench.module_path,
            None,
            None,
            None,
        )?;

        Ok(benchmark_summary)
    }
}

impl Groups {
    /// Create this `Groups` from a [`crate::api::LibraryBenchmark`] submitted by the benchmarking
    /// harness
    fn from_library_benchmark(
        module_path: &ModulePath,
        benchmark_groups: LibraryBenchmarkGroups,
        meta: &Metadata,
    ) -> Result<Self> {
        let global_config = benchmark_groups.config;
        let mut groups = vec![];
        let meta_callgrind_args = meta.args.callgrind_args.clone().unwrap_or_default();

        for library_benchmark_group in benchmark_groups.groups {
            let group_module_path = module_path.join(&library_benchmark_group.id);
            let group_config = global_config
                .clone()
                .update_from_all([library_benchmark_group.config.as_ref()]);

            let setup =
                library_benchmark_group
                    .has_setup
                    .then_some(Assistant::new_group_assistant(
                        AssistantKind::Setup,
                        &library_benchmark_group.id,
                        group_config.collect_envs(),
                    ));
            let teardown =
                library_benchmark_group
                    .has_setup
                    .then_some(Assistant::new_group_assistant(
                        AssistantKind::Teardown,
                        &library_benchmark_group.id,
                        group_config.collect_envs(),
                    ));

            let mut group = Group {
                id: library_benchmark_group.id,
                module_path: group_module_path.clone(),
                compare_by_id: library_benchmark_group
                    .compare_by_id
                    .unwrap_or(defaults::COMPARE_BY_ID),
                benches: vec![],
                setup,
                teardown,
            };

            for (bench_index, library_benchmark_benches) in library_benchmark_group
                .library_benchmarks
                .into_iter()
                .enumerate()
            {
                for (index, library_benchmark_bench) in
                    library_benchmark_benches.benches.into_iter().enumerate()
                {
                    let config = group_config.clone().update_from_all([
                        library_benchmark_benches.config.as_ref(),
                        library_benchmark_bench.config.as_ref(),
                    ]);
                    let envs = config.resolve_envs();

                    let callgrind_args =
                        Args::from_raw_args(&[&config.raw_callgrind_args, &meta_callgrind_args])?;

                    let flamegraph_config = config.flamegraph_config.map(Into::into);
                    let module_path =
                        group_module_path.join(&library_benchmark_bench.function_name);

                    // TODO: Move into LibBench::new
                    let lib_bench = LibBench {
                        bench_index,
                        index,
                        id: library_benchmark_bench.id,
                        function: library_benchmark_bench.function_name,
                        args: library_benchmark_bench.args,
                        entry_point: Some(DEFAULT_TOGGLE.to_owned()),
                        run_options: RunOptions {
                            env_clear: config.env_clear.unwrap_or(true),
                            envs,
                            ..Default::default()
                        },
                        callgrind_args,
                        flamegraph_config,
                        regression_config: api::update_option(
                            &config.regression_config,
                            &meta.regression_config,
                        )
                        .map(Into::into),
                        tools: ToolConfigs(config.tools.0.into_iter().map(Into::into).collect()),
                        module_path,
                        truncate_description: config.truncate_description.unwrap_or(Some(50)),
                    };
                    group.benches.push(lib_bench);
                }
            }

            groups.push(group);
        }

        Ok(Self(groups))
    }

    /// Run all [`LibBench`] benchmarks
    fn run(&self, benchmark: &dyn Benchmark, config: &Config) -> Result<()> {
        let mut is_regressed = false;

        for group in &self.0 {
            if let Some(setup) = &group.setup {
                setup.run(config, &group.module_path)?;
            }

            let mut summaries: HashMap<String, Vec<BenchmarkSummary>> =
                HashMap::with_capacity(group.benches.len());
            for bench in &group.benches {
                let fail_fast = bench
                    .regression_config
                    .as_ref()
                    .map_or(false, |r| r.fail_fast);
                let summary = benchmark.run(bench, config, group)?;
                summary.print_and_save(&config.meta.args.output_format)?;
                summary.check_regression(&mut is_regressed, fail_fast)?;

                if group.compare_by_id && config.meta.args.output_format == OutputFormat::Default {
                    if let Some(id) = &summary.id {
                        if let Some(sums) = summaries.get_mut(id) {
                            for sum in sums.iter() {
                                sum.compare_and_print(id, &config.meta, &summary)?;
                            }
                            sums.push(summary);
                        } else {
                            summaries.insert(id.clone(), vec![summary]);
                        }
                    }
                }
            }

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

impl LibBench {
    /// The name of this `LibBench` consisting of the name of the benchmark function and if present,
    /// the id of the bench attribute (`#[bench::ID(...)]`)
    ///
    /// The name is used to identify a benchmark run within the same [`Group`] and has therefore to
    /// be unique within the same [`Group`]
    fn name(&self) -> String {
        if let Some(bench_id) = &self.id {
            format!("{}.{}", &self.function, bench_id)
        } else {
            self.function.clone()
        }
    }

    /// The arguments for the `bench_bin` to actually run the benchmark function
    fn bench_args(&self, group: &Group) -> Vec<OsString> {
        vec![
            OsString::from("--iai-run".to_owned()),
            OsString::from(&group.id),
            OsString::from(self.bench_index.to_string()),
            OsString::from(self.index.to_string()),
            OsString::from(self.module_path.to_string()),
        ]
    }

    /// This method creates the initial [`BenchmarkSummary`]
    fn create_benchmark_summary(
        &self,
        config: &Config,
        output_path: &ToolOutputPath,
        description: Option<String>,
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
            &self.module_path,
            self.id.clone(),
            description,
            summary_output,
        ))
    }

    /// Check for regressions as defined in [`RegressionConfig`] and print an error if a regression
    /// occurred
    fn check_and_print_regressions(
        &self,
        costs_summary: &CostsSummary,
    ) -> Vec<CallgrindRegressionSummary> {
        if let Some(regression_config) = &self.regression_config {
            regression_config.check_and_print(costs_summary)
        } else {
            vec![]
        }
    }
}

impl Benchmark for LoadBaselineBenchmark {
    fn output_path(&self, lib_bench: &LibBench, config: &Config, group: &Group) -> ToolOutputPath {
        ToolOutputPath::new(
            ToolOutputPathKind::Base(self.loaded_baseline.to_string()),
            ValgrindTool::Callgrind,
            &BaselineKind::Name(self.baseline.clone()),
            &config.meta.target_dir,
            &group.module_path,
            &lib_bench.name(),
        )
    }

    fn baselines(&self) -> (Option<String>, Option<String>) {
        (
            Some(self.loaded_baseline.to_string()),
            Some(self.baseline.to_string()),
        )
    }

    fn run(
        &self,
        lib_bench: &LibBench,
        config: &Config,
        group: &Group,
    ) -> Result<BenchmarkSummary> {
        let header = LibraryBenchmarkHeader::new(&config.meta, lib_bench);
        header.print();

        let bench_args = lib_bench.bench_args(group);
        let sentinel = Sentinel::default();
        let out_path = self.output_path(lib_bench, config, group);
        let old_path = out_path.to_base_path();
        let log_path = out_path.to_log_output();

        let mut benchmark_summary =
            lib_bench.create_benchmark_summary(config, &out_path, header.description())?;

        let new_costs = SentinelParser::new(&sentinel).parse(&out_path)?;
        let old_costs = Some(SentinelParser::new(&sentinel).parse(&old_path)?);
        let costs_summary = CostsSummary::new(&new_costs, old_costs.as_ref());

        VerticalFormat::default().print(&config.meta, self.baselines(), &costs_summary)?;

        let regressions = lib_bench.check_and_print_regressions(&costs_summary);

        let callgrind_summary = benchmark_summary
            .callgrind_summary
            .insert(CallgrindSummary::new(
                log_path.real_paths()?,
                out_path.real_paths()?,
            ));

        callgrind_summary.add_summary(
            &config.bench_bin,
            &bench_args,
            &old_path,
            costs_summary,
            regressions,
        );

        if let Some(flamegraph_config) = lib_bench.flamegraph_config.clone() {
            callgrind_summary.flamegraphs = LoadBaselineFlamegraphGenerator {
                loaded_baseline: self.loaded_baseline.clone(),
                baseline: self.baseline.clone(),
            }
            .create(
                &Flamegraph::new(header.to_title(), flamegraph_config),
                &out_path,
                Some(&sentinel),
                &config.meta.project_root,
            )?;
        }

        benchmark_summary.tool_summaries = lib_bench
            .tools
            .run_loaded_vs_base(&config.meta, &out_path)?;

        Ok(benchmark_summary)
    }
}

impl Runner {
    /// Create a new `Runner`
    fn new(benchmark_groups: LibraryBenchmarkGroups, config: Config) -> Result<Self> {
        let setup = benchmark_groups
            .has_setup
            .then_some(Assistant::new_main_assistant(
                AssistantKind::Setup,
                benchmark_groups.config.collect_envs(),
            ));
        let teardown = benchmark_groups
            .has_teardown
            .then_some(Assistant::new_main_assistant(
                AssistantKind::Teardown,
                benchmark_groups.config.collect_envs(),
            ));

        let groups =
            Groups::from_library_benchmark(&config.module_path, benchmark_groups, &config.meta)?;

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
            config,
            groups,
            benchmark,
            setup,
            teardown,
        })
    }

    /// Run all benchmarks in all groups
    fn run(&self) -> Result<()> {
        if let Some(setup) = &self.setup {
            setup.run(&self.config, &self.config.module_path)?;
        }

        self.groups.run(self.benchmark.as_ref(), &self.config)?;

        if let Some(teardown) = &self.teardown {
            teardown.run(&self.config, &self.config.module_path)?;
        }

        Ok(())
    }
}

impl Benchmark for SaveBaselineBenchmark {
    fn output_path(&self, lib_bench: &LibBench, config: &Config, group: &Group) -> ToolOutputPath {
        ToolOutputPath::new(
            ToolOutputPathKind::Base(self.baseline.to_string()),
            ValgrindTool::Callgrind,
            &BaselineKind::Name(self.baseline.clone()),
            &config.meta.target_dir,
            &group.module_path,
            &lib_bench.name(),
        )
    }

    fn baselines(&self) -> (Option<String>, Option<String>) {
        (
            Some(self.baseline.to_string()),
            Some(self.baseline.to_string()),
        )
    }

    fn run(
        &self,
        lib_bench: &LibBench,
        config: &Config,
        group: &Group,
    ) -> Result<BenchmarkSummary> {
        let header = LibraryBenchmarkHeader::new(&config.meta, lib_bench);
        header.print();

        let callgrind_command = ToolCommand::new(
            ValgrindTool::Callgrind,
            &config.meta,
            config.meta.args.nocapture,
        );

        let mut callgrind_args = lib_bench.callgrind_args.clone();
        if let Some(entry_point) = lib_bench.entry_point.as_ref() {
            callgrind_args.insert_toggle_collect(entry_point);
        }

        let tool_config = ToolConfig::new(ValgrindTool::Callgrind, true, callgrind_args, None);

        let bench_args = lib_bench.bench_args(group);
        let baselines = self.baselines();

        let sentinel = Sentinel::default();
        let out_path = self.output_path(lib_bench, config, group);
        out_path.init()?;

        let old_costs = out_path
            .exists()
            .then(|| {
                SentinelParser::new(&sentinel)
                    .parse(&out_path)
                    .and_then(|costs| out_path.clear().map(|()| costs))
            })
            .transpose()?;

        let log_path = out_path.to_log_output();
        log_path.clear()?;

        let mut benchmark_summary =
            lib_bench.create_benchmark_summary(config, &out_path, header.description())?;

        let output = callgrind_command.run(
            tool_config,
            &config.bench_bin,
            &bench_args,
            lib_bench.run_options.clone(),
            &out_path,
            &lib_bench.module_path,
            None,
        )?;

        print_no_capture_footer(
            config.meta.args.nocapture,
            lib_bench.run_options.stdout.as_ref(),
            lib_bench.run_options.stderr.as_ref(),
        );

        let new_costs = SentinelParser::new(&sentinel).parse(&out_path)?;
        let costs_summary = CostsSummary::new(&new_costs, old_costs.as_ref());
        VerticalFormat::default().print(&config.meta, baselines.clone(), &costs_summary)?;

        output.dump_log(log::Level::Info);
        log_path.dump_log(log::Level::Info, &mut stderr())?;

        let regressions = lib_bench.check_and_print_regressions(&costs_summary);

        let callgrind_summary = benchmark_summary
            .callgrind_summary
            .insert(CallgrindSummary::new(
                log_path.real_paths()?,
                out_path.real_paths()?,
            ));

        callgrind_summary.add_summary(
            &config.bench_bin,
            &bench_args,
            &out_path,
            costs_summary,
            regressions,
        );

        if let Some(flamegraph_config) = lib_bench.flamegraph_config.clone() {
            callgrind_summary.flamegraphs = SaveBaselineFlamegraphGenerator {
                baseline: self.baseline.clone(),
            }
            .create(
                &Flamegraph::new(header.to_title(), flamegraph_config),
                &out_path,
                Some(&sentinel),
                &config.meta.project_root,
            )?;
        }

        benchmark_summary.tool_summaries = lib_bench.tools.run(
            config,
            &config.bench_bin,
            &bench_args,
            &lib_bench.run_options,
            &out_path,
            true,
            &lib_bench.module_path,
            // We don't have a sandbox feature in library benchmarks
            None,
            None,
            None,
        )?;

        Ok(benchmark_summary)
    }
}

/// The top-level method which should be used to initiate running all benchmarks
pub fn run(benchmark_groups: LibraryBenchmarkGroups, config: Config) -> Result<()> {
    Runner::new(benchmark_groups, config)?.run()
}
