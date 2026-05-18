pub const CODING_SPEC_MANUAL: &str = r#"# Clankcord Coding Artifact Spec

Use this workflow when a task asks for code, benchmark programs, performance experiments, disassembly analysis, generated files, or other coding artifacts.

## Tools

The agent runtime provides `clang`, `python`, `zip`, `rg`, `jq`, and the `clankcord` CLI in the execution environment. Use `clang` for C and C-compatible performance probes. Use `python` for scripting, data shaping, plotting inputs, and small reproducible generators.

## Performance Probes

For low-level performance questions, prefer a single-file C program unless the task clearly needs another language. A single translation unit is easy to paste into Compiler Explorer and easy to compile inside the agent workspace.

Write benchmark code so the compiler cannot delete the operation being inspected:

```c
#ifndef CLANKCORD_OBSERVE
#define CLANKCORD_OBSERVE 1
#endif

static volatile unsigned long long clankcord_sink;

static void observe(unsigned long long value) {
#if CLANKCORD_OBSERVE
    clankcord_sink ^= value;
#else
    (void)value;
#endif
}
```

Guard anti-optimization hooks behind macros so the same file can be used for assembly inspection and for local timing. Keep functions small and named after the operation under test. Mark the functions of interest with attributes such as `__attribute__((noinline))` when inlining would hide the machine code being discussed.

For Compiler Explorer, include compile comments at the top of the file:

```c
// clang -O3 -march=x86-64-v3 -S probe.c
// clang -O3 -march=native probe.c -o probe
```

## Packaging

Generated code, benchmark inputs, output captures, and short notes are packaged into a zip file before submission:

```bash
zip -r artifact.zip probe.c README.md results.txt
```

Keep the zip contents focused. Include source files, exact commands, and result files that let a reader reproduce the work. Do not package build directories, dependency caches, secrets, tokens, or large unrelated outputs.

## Submission

Send the response through Clankcord and attach the zip:

```bash
clankcord responses send --attachment artifact.zip <<'EOF'
Short summary of what is attached and what was concluded.
EOF
```

For private delivery, use the same attachment flag with `clankcord responses dm --to ...`. The response body remains required and should summarize the artifact in plain language.
"#;
