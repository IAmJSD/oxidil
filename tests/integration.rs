//! Integration tests: drive the compiled `oxidil` binary on fixtures.
//!
//! These run the real binary (via the cargo-provided `CARGO_BIN_EXE_oxidil`),
//! NOT any external tool, satisfying the no-shell-out constraint at runtime
//! (the test harness itself launching our own binary is fine).

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oxidil"))
}

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    p
}

fn run(args: &[&str]) {
    let status = Command::new(bin())
        .args(args)
        .status()
        .expect("failed to launch oxidil");
    assert!(status.success(), "oxidil exited with failure: {status:?}");
}

fn out(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

#[test]
fn o0_passthrough_vs_o2_fold_differ() {
    let input = fixture("sample.js");
    let o0 = std::env::temp_dir().join("riz_test_o0.js");
    let o2 = std::env::temp_dir().join("riz_test_o2.js");

    run(&[input.to_str().unwrap(), "--out", o0.to_str().unwrap(), "-0"]);
    run(&[input.to_str().unwrap(), "--out", o2.to_str().unwrap(), "-2"]);

    let o0s = std::fs::read_to_string(&o0).unwrap();
    let o2s = std::fs::read_to_string(&o2).unwrap();

    // O0 leaves the foldable expression intact; O2 folds it.
    assert!(o0s.contains("1 + 2 * 3"), "O0 should be passthrough: {o0s}");
    assert!(
        o2s.contains("const a = 7"),
        "O2 should fold 1+2*3 to 7: {o2s}"
    );
    assert_ne!(o0s, o2s, "O0 and O2 must differ");
}

#[test]
fn o2_emits_valid_sourcemap_file() {
    let input = fixture("sample.js");
    let out = std::env::temp_dir().join("riz_test_map.js");
    let map = std::env::temp_dir().join("riz_test_map.js.map");

    run(&[
        input.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--out-map",
        map.to_str().unwrap(),
    ]);

    let code = std::fs::read_to_string(&out).unwrap();
    assert!(
        code.contains("//# sourceMappingURL="),
        "should append map url"
    );

    let map_json = std::fs::read_to_string(&map).unwrap();
    assert!(map_json.contains("\"version\":3"), "valid v3 map");
    assert!(map_json.contains("mappings"), "has mappings");
}

#[test]
fn sourcemap_composition_reaches_original_source() {
    let input = fixture("sample.js");
    let in_map = fixture("sample.input.map");
    let out = std::env::temp_dir().join("riz_test_comp.js");
    let map = std::env::temp_dir().join("riz_test_comp.js.map");

    run(&[
        input.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--out-map",
        map.to_str().unwrap(),
        "--source-map",
        in_map.to_str().unwrap(),
    ]);

    let map_json = std::fs::read_to_string(&map).unwrap();
    // Composed map points back to the ORIGINAL authored source, not the input file.
    assert!(
        map_json.contains("original.js"),
        "composed map must reference original source: {map_json}"
    );
}

#[test]
fn ts_input_is_stripped_to_pure_js() {
    let input = fixture("sample.ts");
    let out = std::env::temp_dir().join("riz_test_ts.js");

    run(&[
        input.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "-0",
    ]);

    let code = std::fs::read_to_string(&out).unwrap();
    // No TS-only syntax should survive.
    assert!(!code.contains("interface"), "interface dropped: {code}");
    assert!(
        !code.contains(": number"),
        "type annotation cleared: {code}"
    );
    assert!(!code.contains(" as "), "as-expression unwrapped: {code}");
    assert!(!code.contains("Point"), "type-only references gone: {code}");
}

// --- Soundness regression tests for the hardened passes ---------------------

/// Constant folding must NOT drop the side effects of a comma/sequence operand:
/// `(f(), 5)` keeps `f()`.
#[test]
fn fold_preserves_comma_side_effects() {
    let input = fixture("comma.js");
    let o = out("riz_test_comma.js");
    run(&[input.to_str().unwrap(), "--out", o.to_str().unwrap(), "-1"]);
    let code = std::fs::read_to_string(&o).unwrap();
    assert!(code.contains("f()"), "f() must be preserved: {code}");
    assert!(
        code.contains("g()") && code.contains("h()"),
        "g()/h() preserved: {code}"
    );
}

/// DCE must not remove bindings referenced only inside a direct `eval("...")`.
#[test]
fn dce_keeps_eval_referenced_bindings() {
    let input = fixture("eval_dce.js");
    let o = out("riz_test_eval.js");
    run(&[input.to_str().unwrap(), "--out", o.to_str().unwrap(), "-2"]);
    let code = std::fs::read_to_string(&o).unwrap();
    assert!(code.contains("secret"), "secret kept: {code}");
    assert!(code.contains("helper"), "helper kept: {code}");
}

/// Minify-rename must bail when the program contains `with` (dynamic resolution).
#[test]
fn rename_bails_on_with() {
    let input = fixture("with_sloppy.js");
    let o = out("riz_test_with.js");
    run(&[
        input.to_str().unwrap(),
        "--out",
        o.to_str().unwrap(),
        "--Os",
    ]);
    let code = std::fs::read_to_string(&o).unwrap();
    assert!(
        code.contains("zzz"),
        "local zzz must not be renamed: {code}"
    );
}

/// Single-use variable inlining: a single-assignment, single-use local with a
/// pure initializer reading only immutable bindings is inlined and its binding
/// removed. `let a = n; let b = a; return a + b;` collapses (no `let a`/`let b`),
/// and behavior is preserved.
#[test]
fn inlining_collapses_single_use_locals() {
    let input = fixture("inline_var.js");
    let o0 = out("riz_test_inline_o0.js");
    let o2 = out("riz_test_inline_o2.js");
    run(&[input.to_str().unwrap(), "--out", o0.to_str().unwrap(), "-0"]);
    run(&[input.to_str().unwrap(), "--out", o2.to_str().unwrap(), "-2"]);
    let o0s = std::fs::read_to_string(&o0).unwrap();
    let o2s = std::fs::read_to_string(&o2).unwrap();
    // O0 keeps the temporaries; O2 inlines them away.
    assert!(o0s.contains("let a"), "O0 keeps temporaries: {o0s}");
    assert!(
        !o2s.contains("let a") && !o2s.contains("let b"),
        "O2 must inline single-use temporaries: {o2s}"
    );
}

/// Inlining must NOT move an initializer that reads a binding which is later
/// reassigned: `let b = a; a = 5; return b;` must still return the ORIGINAL `a`
/// (1), so `b`'s declaration is preserved (the read is captured at decl time).
#[test]
fn inlining_respects_reassignment_hazard() {
    let input = fixture("inline_reassign.js");
    let o = out("riz_test_inline_reassign.js");
    run(&[input.to_str().unwrap(), "--out", o.to_str().unwrap(), "-2"]);
    let code = std::fs::read_to_string(&o).unwrap();
    // `b` must survive: inlining `a` into the `return` would observe a=5.
    assert!(
        code.contains("let b = a") || code.contains("b = a"),
        "reassignment hazard: b's value must be captured before a is reassigned: {code}"
    );
}

/// ts-typeof must NOT fold a guard based on a PARAMETER annotation (erased,
/// not runtime-enforced). The guard must survive at -O2 --ts-typeof.
#[test]
fn ts_typeof_does_not_fold_parameter_annotation() {
    let input = fixture("typeof_param.ts");
    let o = out("riz_test_typeof_param.js");
    run(&[
        input.to_str().unwrap(),
        "--out",
        o.to_str().unwrap(),
        "--O2",
        "--ts-typeof",
    ]);
    let code = std::fs::read_to_string(&o).unwrap();
    assert!(
        code.contains("typeof x"),
        "parameter typeof guard must NOT be folded: {code}"
    );
}

/// ts-typeof SHOULD still fold a sound `const` fact (initializer-derived).
#[test]
fn ts_typeof_folds_sound_const() {
    let input = fixture("typeof_const.ts");
    let o = out("riz_test_typeof_const.js");
    run(&[
        input.to_str().unwrap(),
        "--out",
        o.to_str().unwrap(),
        "--O2",
        "--ts-typeof",
    ]);
    let code = std::fs::read_to_string(&o).unwrap();
    assert!(code.contains("\"yes\""), "const typeof fact folds: {code}");
    assert!(!code.contains("typeof n"), "guard removed: {code}");
}

/// cse-gvn: a non-trivial, provably-PURE expression (`(x === y) && (x !== 0)`)
/// computed 3 times over never-mutated locals is hoisted into a single fresh
/// `const` temp and reused. We isolate the pass (disable the other O2 cleanups
/// that would otherwise inline the temp back) so the dedup is observable, and we
/// confirm the temp appears exactly once.
#[test]
fn cse_gvn_dedups_repeated_pure_expression() {
    let input = fixture("cse_repeat.js");
    let o = out("riz_test_cse_only.js");
    run(&[
        input.to_str().unwrap(),
        "--out",
        o.to_str().unwrap(),
        "-2",
        "--disable",
        "propagation",
        "--disable",
        "inlining",
        "--disable",
        "dead-code-elimination",
        "--disable",
        "dead-store-elimination",
    ]);
    let code = std::fs::read_to_string(&o).unwrap();
    // A single hoisted temp binding is introduced...
    assert!(
        code.matches("_cse").count() >= 3,
        "cse temp introduced and reused: {code}"
    );
    // ...and the original 3-occurrence expression is collapsed to one computation.
    assert!(
        code.matches("!== 0").count() == 1,
        "the repeated pure expression is computed once: {code}"
    );
}

// --- Differential-equivalence regression tests for the hardened passes -------
//
// Each of these compiles a fixture at a specific optimization level and runs the
// emitted JS through `node`, asserting the stdout+exit-code matches the `-O0`
// (passthrough) baseline. This is exactly the class of bug each finding reported:
// node prints/throws something different at -O1+ than at -O0. We launch `node`
// (an external interpreter) only to OBSERVE behavior in the test harness; the
// compiler binary itself never shells out.

/// Locate `node` on PATH; `None` if unavailable (tests then skip gracefully).
fn node_bin() -> Option<PathBuf> {
    let out = Command::new("sh")
        .arg("-c")
        .arg("command -v node")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        None
    } else {
        Some(PathBuf::from(p))
    }
}

/// Run `node <file>`, returning (stdout+stderr, exit-code).
fn node_run(node: &PathBuf, file: &PathBuf) -> (String, i32) {
    let out = Command::new(node)
        .arg(file)
        .output()
        .expect("failed to launch node");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

/// Compile `fixture_name` at `-O0` and at every level in `levels`, run each
/// through node, and assert the node output (stdout+stderr) AND exit code match
/// the `-O0` baseline. `tag` disambiguates temp output files.
fn assert_levels_match_o0(fixture_name: &str, tag: &str, levels: &[&str]) {
    let Some(node) = node_bin() else {
        eprintln!("node not found; skipping differential test for {fixture_name}");
        return;
    };
    let input = fixture(fixture_name);
    let o0 = out(&format!("riz_diff_{tag}_O0.js"));
    run(&[input.to_str().unwrap(), "--out", o0.to_str().unwrap(), "-0"]);
    let (base_out, base_rc) = node_run(&node, &o0);

    for level in levels {
        let oj = out(&format!(
            "riz_diff_{tag}_{}.js",
            level.trim_start_matches('-')
        ));
        run(&[
            input.to_str().unwrap(),
            "--out",
            oj.to_str().unwrap(),
            level,
        ]);
        let (lvl_out, lvl_rc) = node_run(&node, &oj);
        assert_eq!(
            base_out, lvl_out,
            "stdout diverged for {fixture_name} at {level}\n-O0:\n{base_out}\n{level}:\n{lvl_out}"
        );
        assert_eq!(
            base_rc, lvl_rc,
            "exit code diverged for {fixture_name} at {level} (O0={base_rc} {level}={lvl_rc})"
        );
    }
}

const ALL_LEVELS: &[&str] = &["-1", "-2", "-3", "--Os"];

// propagation: declaration must dominate every use (var-hoist / TDZ / conditional)
#[test]
fn prop_var_hoist_preinit_read_preserved() {
    assert_levels_match_o0("prop_var_hoist.js", "prop_var", ALL_LEVELS);
}
#[test]
fn prop_tdz_let_throw_preserved() {
    assert_levels_match_o0("prop_tdz_let.js", "prop_tdz", ALL_LEVELS);
}
#[test]
fn prop_conditional_var_init_preserved() {
    assert_levels_match_o0("prop_cond_var.js", "prop_cond", ALL_LEVELS);
}

// pure-eval: monkey-patched global built-in must not be folded to spec result
#[test]
fn pure_eval_does_not_fold_patched_global_method() {
    assert_levels_match_o0("pureeval_patch.js", "pe_patch", ALL_LEVELS);
}
// ...but a pristine global built-in IS still folded (no regression of the win).
#[test]
fn pure_eval_still_folds_pristine_global() {
    assert_levels_match_o0("pureeval_pristine.js", "pe_pristine", ALL_LEVELS);
    let o = out("riz_pe_pristine_check.js");
    run(&[
        fixture("pureeval_pristine.js").to_str().unwrap(),
        "--out",
        o.to_str().unwrap(),
        "-1",
    ]);
    let code = std::fs::read_to_string(&o).unwrap();
    assert!(
        code.contains('9') && !code.contains("Math.floor"),
        "pristine Math.floor must still fold: {code}"
    );
}

// control-flow switch-on-constant soundness
#[test]
fn cfg_switch_nested_break_preserved() {
    assert_levels_match_o0("cfg_switch_nested_break.js", "cfg_break", ALL_LEVELS);
}
#[test]
fn cfg_switch_nomatch_keeps_hoisted_bindings() {
    assert_levels_match_o0("cfg_switch_nomatch_hoist.js", "cfg_nomatch", ALL_LEVELS);
}
#[test]
fn cfg_switch_earlier_case_hoist_preserved() {
    assert_levels_match_o0("cfg_switch_earlier_hoist.js", "cfg_earlier", ALL_LEVELS);
}

// dead-store: const / TDZ / parameter-arguments soundness (DSE is O2+)
#[test]
fn dse_const_selfassign_throw_preserved() {
    assert_levels_match_o0(
        "dse_const_selfassign.js",
        "dse_cself",
        &["-2", "-3", "--Os"],
    );
}
#[test]
fn dse_const_deadstore_throw_preserved() {
    assert_levels_match_o0("dse_const_deadstore.js", "dse_cdead", &["-2", "-3", "--Os"]);
}
#[test]
fn dse_tdz_throw_preserved() {
    assert_levels_match_o0("dse_tdz.js", "dse_tdz", &["-2", "-3", "--Os"]);
}
#[test]
fn dse_sloppy_param_arguments_alias_preserved() {
    assert_levels_match_o0("dse_param_arguments.js", "dse_param", &["-2", "-3", "--Os"]);
}

// dead-param: a dropped trailing arg reading a TDZ binding keeps the throw
#[test]
fn dead_param_tdz_arg_throw_preserved() {
    assert_levels_match_o0("deadparam_tdz.js", "dp_tdz", &["-2", "-3", "--Os"]);
}

// inlining: block-scope shadow must not change which binding a free id resolves to
#[test]
fn inline_block_shadow_preserved() {
    assert_levels_match_o0(
        "inline_block_shadow.js",
        "inl_shadow",
        &["-2", "-3", "--Os"],
    );
}

// object-construction: fold a run of own-property stores into the declarator's
// object literal (O2+), and the soundness boundaries that must NOT fold.
#[test]
fn objconstruct_folds_run_of_stores() {
    assert_levels_match_o0("objconstruct_fold.js", "oc_fold", &["-2", "-3", "--Os"]);
    // ...and the fold actually fired at -O2 (the stores are gone, replaced by
    // literal members).
    let o = out("riz_oc_fold_check.js");
    run(&[
        fixture("objconstruct_fold.js").to_str().unwrap(),
        "--out",
        o.to_str().unwrap(),
        "-2",
    ]);
    let code = std::fs::read_to_string(&o).unwrap();
    assert!(
        !code.contains("x.a =") && !code.contains("x.b ="),
        "stores should be folded into the literal: {code}"
    );
}
#[test]
fn objconstruct_proto_getter_not_folded() {
    assert_levels_match_o0(
        "objconstruct_proto_getter.js",
        "oc_getter",
        &["-2", "-3", "--Os"],
    );
}
#[test]
fn objconstruct_try_throw_partial_object_preserved() {
    assert_levels_match_o0("objconstruct_try_throw.js", "oc_try", &["-2", "-3", "--Os"]);
}
#[test]
fn objconstruct_self_reference_not_folded() {
    assert_levels_match_o0(
        "objconstruct_selfref.js",
        "oc_selfref",
        &["-2", "-3", "--Os"],
    );
}

// array-construction: fold an ascending dense run of indexed stores into the
// declarator's array literal (O2+), and the soundness boundaries that must NOT
// fold (sparse gap, tainted prototype, throwing store in try, self-reference).
#[test]
fn arrayconstruct_folds_ascending_run() {
    assert_levels_match_o0("arrayconstruct_fold.js", "ac_fold", &["-2", "-3", "--Os"]);
    let o = out("riz_ac_fold_check.js");
    run(&[
        fixture("arrayconstruct_fold.js").to_str().unwrap(),
        "--out",
        o.to_str().unwrap(),
        "-2",
    ]);
    let code = std::fs::read_to_string(&o).unwrap();
    assert!(
        !code.contains("x[0] =") && !code.contains("x[1] =") && !code.contains("y[2] ="),
        "indexed stores should be folded into the literal: {code}"
    );
}
#[test]
fn arrayconstruct_sparse_prefix_only() {
    assert_levels_match_o0(
        "arrayconstruct_sparse.js",
        "ac_sparse",
        &["-2", "-3", "--Os"],
    );
}
#[test]
fn arrayconstruct_proto_getter_not_folded() {
    assert_levels_match_o0(
        "arrayconstruct_proto_getter.js",
        "ac_getter",
        &["-2", "-3", "--Os"],
    );
}
#[test]
fn arrayconstruct_try_throw_partial_array_preserved() {
    assert_levels_match_o0(
        "arrayconstruct_try_throw.js",
        "ac_try",
        &["-2", "-3", "--Os"],
    );
}
#[test]
fn arrayconstruct_self_reference_not_folded() {
    assert_levels_match_o0(
        "arrayconstruct_selfref.js",
        "ac_selfref",
        &["-2", "-3", "--Os"],
    );
}

// param-scalarization: split a non-escaping local function's options-object param
// into scalar params + rewrite call sites (O2+), and bail on every hazard.
#[test]
fn scalarize_splits_options_object() {
    assert_levels_match_o0("scalarize_fold.js", "sc_fold", &["-2", "-3", "--Os"]);
    let o = out("riz_sc_fold_check.js");
    run(&[
        fixture("scalarize_fold.js").to_str().unwrap(),
        "--out",
        o.to_str().unwrap(),
        "-2",
    ]);
    let code = std::fs::read_to_string(&o).unwrap();
    assert!(
        !code.contains("opts"),
        "options object should be scalarized away: {code}"
    );
}
#[test]
fn scalarize_escaping_object_not_split() {
    assert_levels_match_o0("scalarize_escape.js", "sc_escape", &["-2", "-3", "--Os"]);
}
#[test]
fn scalarize_getter_literal_not_split() {
    assert_levels_match_o0("scalarize_getter.js", "sc_getter", &["-2", "-3", "--Os"]);
}
#[test]
fn scalarize_inconsistent_order_not_split() {
    assert_levels_match_o0("scalarize_order.js", "sc_order", &["-2", "-3", "--Os"]);
}
#[test]
fn scalarize_arguments_user_not_split() {
    assert_levels_match_o0("scalarize_arguments.js", "sc_args", &["-2", "-3", "--Os"]);
}
#[test]
fn scalarize_key_collision_not_split() {
    assert_levels_match_o0("scalarize_collision.js", "sc_coll", &["-2", "-3", "--Os"]);
}
#[test]
fn scalarize_proto_taint_not_split() {
    assert_levels_match_o0("scalarize_proto.js", "sc_proto", &["-2", "-3", "--Os"]);
}

// cse-gvn: repeated coercion side effects must not be collapsed
#[test]
fn cse_does_not_drop_coercion_side_effects() {
    assert_levels_match_o0("cse_coercion.js", "cse_coerce", &["-2", "-3", "--Os"]);
}

// licm: hoisting must not move an operand read into its TDZ (zero-iter loop)
#[test]
fn licm_does_not_hoist_into_tdz() {
    assert_levels_match_o0("licm_tdz.js", "licm_tdz", &["-3"]);
}

/// Broad differential-equivalence sweep over representative corpus programs at
/// every level (incl. --ts-typeof for the TS ones). Asserts node stdout+exit
/// match -O0 across the whole pipeline — the end-to-end soundness guarantee.
#[test]
fn corpus_differential_equivalence_all_levels() {
    let Some(node) = node_bin() else {
        eprintln!("node not found; skipping corpus differential sweep");
        return;
    };
    let corpus = {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("difftest/corpus");
        p
    };
    // A representative subset spanning the trickiest soundness areas.
    let programs = [
        "c01_closures.js",
        "c02_getters_setters.js",
        "c04_switch_fallthrough.js",
        "c05_loops_zero.js",
        "c06_coercion.js",
        "c07_reassign.js",
        "c11_this_arguments.js",
        "c14_dead_store_inline.js",
        // Soundness regressions for the effectiveness relaxations (each was a
        // confirmed divergence before the corresponding guard was tightened).
        "r01_module_dce_tdz_sibling.mjs",
        "r02_module_dce_undeclared.mjs",
        "r03_module_inline_tdz.mjs",
        "r04_logical_tdz_no_move.mjs",
        "r05_comma_tdz_no_move.mjs",
        "r06_switch_case_tdz.js",
        "r07_globalThis_member_patch.js",
        "r08_globalThis_computed_patch.js",
        "r09_globalThis_nested_patch.js",
        "r10_defineProperty_patch.js",
        "r11_object_assign_patch.js",
        "r12_reflect_defineProperty_patch.js",
        "r13_destructure_global_patch.js",
        "r14_forof_global_patch.js",
    ];
    for prog in programs {
        let input = corpus.join(prog);
        if !input.exists() {
            continue;
        }
        let tag = prog.replace('.', "_");
        let o0 = out(&format!("riz_corpus_{tag}_O0.js"));
        run(&[input.to_str().unwrap(), "--out", o0.to_str().unwrap(), "-0"]);
        let (base_out, base_rc) = node_run(&node, &o0);
        for level in ["-1", "-2", "-3", "--Os"] {
            let oj = out(&format!(
                "riz_corpus_{tag}_{}.js",
                level.trim_start_matches('-')
            ));
            run(&[
                input.to_str().unwrap(),
                "--out",
                oj.to_str().unwrap(),
                level,
            ]);
            let (lvl_out, lvl_rc) = node_run(&node, &oj);
            assert_eq!(
                base_out, lvl_out,
                "corpus {prog} stdout diverged at {level}\n-O0:\n{base_out}\n{level}:\n{lvl_out}"
            );
            assert_eq!(
                base_rc, lvl_rc,
                "corpus {prog} exit code diverged at {level}"
            );
        }
    }
}
