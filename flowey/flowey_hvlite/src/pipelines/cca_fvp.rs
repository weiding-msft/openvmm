// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! See [`CcaFvpCli`]

use flowey::node::prelude::ReadVar;
use flowey::pipeline::prelude::*;
use std::path::PathBuf;

/// Build the ARM64 CCA paravisor and run an execution test of the hypervisor,
/// paravisor, and guest using Arm’s Fixed Virtual Platform (FVP) which is a
/// AArch64 software emulator. If FVP is not installed, it will be downloaded
/// and built via Arm’s official FVP deployment system, 'shrinkwrap'.
#[derive(clap::Args)]
pub struct CcaFvpCli {
    /// Directory to store all files related to CCA FVP–based tests.
    #[clap(long, default_value = "target/cca-fvp")]
    pub dir: PathBuf,

    /// Platform YAML consumed by 'shrinkwrap', absolute path or file name
    /// (searched under shrinkwrap config dir) are accepted. 'platform' is fixed
    /// into cca-3world for CCA FVP test, so this option is only needed for
    /// debug purpose, therefore hide it from user. A few other options are
    /// hidden for the same reason.
    #[clap(long, default_value = "cca-3world.yaml", hide = true)]
    pub platform: PathBuf,

    /// Overlay YAMLs (repeatable) consumed by 'shrinkwrap', absolute path or
    /// file name (searched under shrinkwrap config dir) are accepted. Defaults:
    ///
    ///    --overlay buildroot.yaml --overlay planes.yaml
    #[clap(long, hide = true)]
    pub overlay: Option<Vec<PathBuf>>,

    /// Build-time variables (repeatable) consumed by 'shrinkwrap'. Defaults:
    ///
    ///    --btvar 'GUEST_ROOTFS=${artifact:BUILDROOT}'
    #[clap(long, hide = true)]
    pub btvar: Option<Vec<String>>,

    /// Rootfs path to pass at runtime consumed by'shrinkwrap', absolute path or
    /// file name (searched under shrinkwrap package dir) are accepted
    #[clap(long, default_value = "rootfs.ext2", hide = true)]
    pub rootfs: PathBuf,

    /// Additional runtime variables (repeatable) consumed by 'shrinkwrap',
    /// e.g. --rtvar FOO=bar
    #[clap(long, hide = true)]
    pub rtvar: Option<Vec<String>>,

    /// Automatically install missing deps (requires sudo on Ubuntu)
    #[clap(long, default_value_t = true)]
    pub install_missing_deps: bool,

    /// If repo already exists, attempt `git pull --ff-only`
    #[clap(long, default_value_t = true, hide = true)]
    pub update_shrinkwrap_repo: bool,

    /// Verbose pipeline output
    #[clap(long, default_value_t = false)]
    pub verbose: bool,
}

impl IntoPipeline for CcaFvpCli {
    fn into_pipeline(self, backend_hint: PipelineBackendHint) -> anyhow::Result<Pipeline> {
        let Self {
            dir,
            platform,
            overlay,
            btvar,
            rootfs,
            rtvar,
            install_missing_deps,
            update_shrinkwrap_repo,
            verbose,
        } = self;

        let openvmm_repo = flowey_lib_common::git_checkout::RepoSource::ExistingClone(
            ReadVar::from_static(crate::repo_root()),
        );

        let mut pipeline = Pipeline::new();

        // Convert dir to absolute path to ensure consistency across jobs
        // Relative paths are resolved from the repository root. Use match
        // guards to keep the code simple and clean.
        //
        // shrinkwrap source code and all generated files during FVP and
        // CCA software stack installation are kept under 'dir'
        let dir = match std::fs::canonicalize(&dir) {
            Ok(p) => p,
            Err(_) if dir.is_absolute() => dir.clone(),
            Err(_) => crate::repo_root().join(&dir),
        };

        // Put everything related with 'shrinkwrap' into 'dir'
        let shrinkwrap_src_dir = dir.join("shrinkwrap-src");
        let shrinkwrap_config_dir = shrinkwrap_src_dir.join("config");
        let platform_name = platform.with_extension("");
        let shrinkwrap_package_dir = dir.join("shrinkwrap-build").join(platform_name).join("package");

        // Helper to check and resolve path into absolute paths or $search_path/filename.
        let resolve_path = |p: PathBuf, ctx_name: &str, search_path: PathBuf| -> anyhow::Result<PathBuf> {
            if p.is_absolute() {
                return Ok(p);
            }

            if p.components().count() == 1 {
                return Ok(search_path.join(p));
            }

            anyhow::bail!("{}: only accept absolute path or filename, but {} is received.", ctx_name, p.display())
        };

        // Resolve or initialize a few options
        let platform = resolve_path(platform, "--platform", shrinkwrap_config_dir.clone())?;

        let overlay: Vec<PathBuf> = overlay
            .unwrap_or_else(|| vec![
                PathBuf::from("buildroot.yaml"),
                PathBuf::from("planes.yaml"),
            ])
            .into_iter()
            .map(|p| resolve_path(p, "--overlay", shrinkwrap_config_dir.clone()))
            .collect::<anyhow::Result<_>>()?;

        let btvar = btvar.unwrap_or_else(|| { vec!["GUEST_ROOTFS=${artifact:BUILDROOT}".to_string()] });

        let rootfs = resolve_path(rootfs, "--rootfs", shrinkwrap_package_dir.clone());

        /*
        let rootfs = rootfs.unwrap_or_else(|| {
            // First try SHRINKWRAP_PACKAGE env var, then HOME env var
            let base_path = std::env::var("SHRINKWRAP_PACKAGE")
                .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.shrinkwrap/package", h)))
                .expect("Either SHRINKWRAP_PACKAGE or HOME environment variable must be set");
            PathBuf::from(format!("{}/cca-3world/rootfs.ext2", base_path))
        }); */

        // Create separate jobs to ensure proper ordering
        let install_job = pipeline
            .new_job(
                FlowPlatform::host(backend_hint),
                FlowArch::host(backend_hint),
                "cca-fvp: install shrinkwrap",
            )
            .dep_on(|_| flowey_lib_hvlite::_jobs::cfg_versions::Request::Init)
            .dep_on(|_| flowey_lib_hvlite::_jobs::cfg_hvlite_reposource::Params {
                hvlite_repo_source: openvmm_repo.clone(),
            })
            .dep_on(|_| flowey_lib_hvlite::_jobs::cfg_common::Params {
                local_only: Some(flowey_lib_hvlite::_jobs::cfg_common::LocalOnlyParams {
                    interactive: true,
                    auto_install: install_missing_deps,
                    force_nuget_mono: false,
                    external_nuget_auth: false,
                    ignore_rust_version: true,
                }),
                verbose: ReadVar::from_static(verbose),
                locked: false,
                deny_warnings: false,
            })
            .dep_on(|ctx| flowey_lib_hvlite::_jobs::local_install_shrinkwrap::Params {
                shrinkwrap_src_dir: shrinkwrap_src_dir.clone(),
                do_installs: install_missing_deps,
                update_repo: update_shrinkwrap_repo,
                done: ctx.new_done_handle(),
            })
            .finish();

        let build_job = pipeline
            .new_job(
                FlowPlatform::host(backend_hint),
                FlowArch::host(backend_hint),
                "cca-fvp: shrinkwrap build",
            )
            .dep_on(|_| flowey_lib_hvlite::_jobs::cfg_versions::Request::Init)
            .dep_on(|_| flowey_lib_hvlite::_jobs::cfg_hvlite_reposource::Params {
                hvlite_repo_source: openvmm_repo.clone(),
            })
            .dep_on(|_| flowey_lib_hvlite::_jobs::cfg_common::Params {
                local_only: Some(flowey_lib_hvlite::_jobs::cfg_common::LocalOnlyParams {
                    interactive: true,
                    auto_install: install_missing_deps,
                    force_nuget_mono: false,
                    external_nuget_auth: false,
                    ignore_rust_version: true,
                }),
                verbose: ReadVar::from_static(verbose),
                locked: false,
                deny_warnings: false,
            })
            .dep_on(|ctx| flowey_lib_hvlite::_jobs::local_shrinkwrap_build::Params {
                out_dir: dir.clone(),
                shrinkwrap_src_dir: shrinkwrap_src_dir.clone(),
                platform_yaml: platform.clone(),
                overlays: overlay.clone(),
                btvars: btvar.clone(),
                done: ctx.new_done_handle(),
            })
            .finish();

        // Shrinkwrap run job
        let run_job = pipeline
            .new_job(
                FlowPlatform::host(backend_hint),
                FlowArch::host(backend_hint),
                "cca-fvp: shrinkwrap run",
            )
            .dep_on(|_| flowey_lib_hvlite::_jobs::cfg_versions::Request::Init)
            .dep_on(|_| flowey_lib_hvlite::_jobs::cfg_hvlite_reposource::Params {
                hvlite_repo_source: openvmm_repo.clone(),
            })
            .dep_on(|_| flowey_lib_hvlite::_jobs::cfg_common::Params {
                local_only: Some(flowey_lib_hvlite::_jobs::cfg_common::LocalOnlyParams {
                    interactive: true,
                    auto_install: install_missing_deps,
                    force_nuget_mono: false,
                    external_nuget_auth: false,
                    ignore_rust_version: true,
                }),
                verbose: ReadVar::from_static(verbose),
                locked: false,
                deny_warnings: false,
            })
            .dep_on(|ctx| flowey_lib_hvlite::_jobs::local_shrinkwrap_run::Params {
                out_dir: dir.clone(),
                shrinkwrap_src_dir: shrinkwrap_src_dir.clone(),
                platform_yaml: platform.clone(),
                rootfs_path: rootfs.clone(),
                rtvars: rtvar.clone(),
                done: ctx.new_done_handle(),
            })
            .finish();

        // Explicitly declare job dependencies
        pipeline.non_artifact_dep(&build_job, &install_job);
        pipeline.non_artifact_dep(&run_job, &build_job);
        Ok(pipeline)
    }
}
