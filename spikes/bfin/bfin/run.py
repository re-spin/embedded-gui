"""Build the embedded-gui spike for bfin-cdj-gui, link a standalone ELF (no
panic waiver), apply the GUI gates, run it in bfin-elf-run and report.

Usage: python3 spikes/bfin/bfin/run.py [--opt 2|s]   (default 2: the mods GUI firmware
profile; "s" maps to -O1 under codegen_gcc)
Needs a cdj-2000nxs-mods checkout (CDJ_MODS, default ../cdj-2000nxs-mods) for
the target spec, bench runtime, linker script and tools/build.py; read only.
Simulator cycles are not hardware timing: no cache or SDRAM latency model.
"""
import argparse
import json
import os
import re
import struct
import subprocess
import sys
from collections import Counter
from pathlib import Path

SPIKE = Path(__file__).resolve().parents[1]
MODS = Path(os.environ.get("CDJ_MODS", SPIKE.parents[2] / "cdj-2000nxs-mods")).resolve()
OUT = SPIKE / "target/bfin"
sys.path.insert(0, str(MODS / "tools"))
from build import bfin_frame_sizes  # noqa: E402

TC = Path(os.environ.get("CDJ_TOOLCHAIN_ROOT", "~/.local/share/cdj-toolchain")).expanduser()
NM, OBJDUMP = (TC / f"binutils-bfin/bin/bfin-elf-{t}" for t in ("nm", "objdump"))
FB = 480 * 234 * 2
MHZ = 393
LABELS = ["browse: build", "browse: full frame", "browse: 1 row text (render_dirty)",
          "browse: selection 0->1 (render_dirty, 2 rows)", "player: build", "player: full frame",
          "player: frames digits (render_dirty)", "player: 6 raw images 24x24 (render_dirty)", "ref: raw fill 480x234 (no embedded-gui)",
          "ref: build 1 panel", "ref: 1 full-screen panel (render)", "ref: build 1 label",
          "ref: 1 row label, 21 glyphs 9x18 (render)", "ref: build icons", "ref: 6 raw images 24x24 alone (render)",
          "ref: build A4 text", "ref: 20 glyph-like A4 masks 24x24 on a 240x60 pane (render)",
          "ref: build big indexed8", "ref: 400x90 indexed8, 70% transparent index 0 (render)",
          "ref: build big indexed8 (255)", "ref: 400x90 indexed8, 70% transparent index 255 (render)",
          "ref: build wave", "ref: waveform alone, 400 column fill_rects (render)",
          "ref: build same label, no text",
          "ref: same label, no text (render)"]
GLYPHS = 21  # glyphs in the ref row label
# Panic roots: core's panic entry points and the cold helpers that call them.
PANIC = re.compile(r"panic|unwrap_failed|expect_failed|_fail\d|_fail$|slice_error_fail|rust_begin_unwind|unwind")


def run(*argv, **kw):
    return subprocess.run([str(a) for a in argv], check=True, **kw)


def crate_of(sym):
    """Crate of the first path in a v0-mangled name (_R...C<disambig>_<len><crate>),
    i.e. where the item (or the generic, for an instantiation) is defined."""
    if not sym.startswith("_R"):
        return "compiler_builtins/C" if sym.startswith(("__", "_mem")) else "C (bench)"
    m = re.search(r"C(?:s[0-9A-Za-z]*_)?(\d+)([a-z_][a-z0-9_]*)", sym)
    return m.group(2)[: int(m.group(1))] if m else "?"


def short(sym):
    """Demangled name without hashes/generic args (bfin-elf-c++filt knows v0)."""
    d = subprocess.run([str(TC / "binutils-bfin/bin/bfin-elf-c++filt")], input=sym, capture_output=True,
                       text=True).stdout.strip()
    d = re.sub(r"\[[0-9a-f]{16}\]", "", d)
    while (n := re.sub(r"<[^<>]*>", "", d)) != d:
        d = n
    return d[:90]


def link(elf, objs, lib, stub, *extra):
    obj = OUT / "softfloat_count.o"
    run(TC / "gcc-bfin/bin/bfin-elf-gcc", "-mcpu=bf531", "-c", stub, "-o", obj)
    run(TC / "binutils-bfin/bin/bfin-elf-ld", "--no-warn-rwx-segments", "--gc-sections", *extra,
        "-T", MODS / "tests/bfin/gui_copy_test.ld", "-Map", elf.with_suffix(".map"), "-o", elf, *objs, obj, lib)
    # ponytail: no libgcc here (this cdj-toolchain gcc ships none); the firmware link
    # appends it after the staticlib, where compiler_builtins already wins.


def softfloat_stubs(helpers):
    """__wrap_X: count the call in _sf_calls[i], then tail-jump to X. Only P0
    and R3 are touched (scratch, not argument registers)."""
    lines = [".data", ".global _sf_n", f"_sf_n: .long {len(helpers)}", ".global _sf_calls",
             f"_sf_calls: .space {4 * max(len(helpers), 1)}", ".text"]
    for i, h in enumerate(helpers):
        lines += [f".global ___wrap_{h[1:]}", f"___wrap_{h[1:]}:",
                  "P0.L = _sf_calls; P0.H = _sf_calls;", f"R3 = [P0 + {4 * i}];", "R3 += 1;",
                  f"[P0 + {4 * i}] = R3;", f"JUMP.L ___real_{h[1:]};"]
    return "\n".join(lines) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--opt", default="2", help="gui-firmware opt-level (2 or s)")
    ap.add_argument("--lines", action="store_true", help="build with line tables, print each panic site's source line")
    args = ap.parse_args()
    opt = args.opt
    # Host frames from the same tree, so the identity check is against this build.
    run("cargo", "+nightly", "run", "-q", "--release", "--bin", "host", SPIKE / "target/frames", cwd=SPIKE)
    env = dict(os.environ, CARGO_PROFILE_GUI_FIRMWARE_OPT_LEVEL=opt)
    if args.lines:
        env["CARGO_PROFILE_GUI_FIRMWARE_DEBUG"] = "line-tables-only"
    run("cargo", "rustc", "-q", "--profile", "gui-firmware", "--lib", "--crate-type", "staticlib",
        "--target", MODS / ".sdk/targets/bfin-cdj-gui.json", "-Zjson-target-spec",
        "-Zbuild-std=core,compiler_builtins", "-Zbuild-std-features=compiler-builtins-mem", cwd=SPIKE, env=env)
    lib = SPIKE / "target/bfin-cdj-gui/gui-firmware/libegui_bfin_spike.a"
    OUT.mkdir(parents=True, exist_ok=True)
    objs = []
    for src in (MODS / "tests/bfin/bench_rt.S", SPIKE / "bfin/main.c"):
        obj = OUT / (src.stem + ".o")
        run(TC / "gcc-bfin/bin/bfin-elf-gcc", "-mcpu=bf531", "-Os", "-ffreestanding", "-fno-builtin",
            "-Wall", "-Wextra", "-Werror", "-c", src, "-o", obj)
        objs.append(obj)
    # The firmware's row fill (_cdj_gui_fill16); --gc-sections drops the rest of the file.
    copy = OUT / "copy.o"
    run(TC / "gcc-bfin/bin/bfin-elf-gcc", "-mcpu=bf531", "-c", MODS / "rust/gui/presentation/asm/copy.S", "-o", copy)
    objs.append(copy)
    elf = OUT / "spike.elf"
    # Pass 1: the real link (sizes, gates). Pass 2 (sim only): every soft-float
    # helper pass 1 linked is wrapped with a call counter (ld --wrap).
    stub = OUT / "softfloat_count.S"
    stub.write_text(".data\n.global _sf_n\n_sf_n: .long 0\n.global _sf_calls\n_sf_calls: .long 0\n")
    link(elf, objs, lib, stub)
    names = {l.split()[-1] for l in run(NM, elf, capture_output=True, text=True).stdout.splitlines()}
    softfloat = sorted(n for n in names if re.fullmatch(r"___[a-z]+[sd]f[23]", n) or re.fullmatch(r"___(?:fix|float|extend|trunc)\w+", n))
    stub.write_text(softfloat_stubs(softfloat))
    counted = OUT / "spike-counted.elf"
    link(counted, objs, lib, stub, *(f"--wrap={n[1:]}" for n in softfloat))  # C names: bfin prefixes "_"

    symbols = run(NM, "-S", "--size-sort", elf, capture_output=True, text=True).stdout
    wide = sorted(n for n in names if re.search(r"ti3$|di3$", n))
    frames = bfin_frame_sizes(OBJDUMP, elf)
    big = sorted(((v, k) for k, v in frames.items() if v > 256), reverse=True)
    sizes = run(OBJDUMP, "-h", elf, capture_output=True, text=True).stdout
    sections = {m[0]: int(m[1], 16) for m in re.findall(r"^\s*\d+\s+(\.\w+)\s+([0-9a-f]+)", sizes, re.M)}

    # Panic call sites: every CALL/JUMP from a non-panic function into a panic root.
    listing = run(OBJDUMP, "-d", "--no-show-raw-insn", elf, capture_output=True, text=True).stdout
    sites, fn, site_addrs = Counter(), None, []
    soft_callers = Counter()
    for line in listing.splitlines():
        if (m := re.match(r"[0-9a-f]+ <(_[^>]+)>:$", line)):
            fn = m.group(1)
            continue
        if fn and (m := re.search(r"(?:CALL|JUMP(?:\.[LS])?)\s+0x[0-9a-f]+ <(_[^>+]+)>", line)):
            if PANIC.search(m.group(1)) and not PANIC.search(fn):
                sites[fn] += 1
                site_addrs.append(line.split(":")[0].strip())
            if m.group(1) in softfloat:
                soft_callers[fn] += 1
    by_crate = Counter()
    for f, c in sites.items():
        by_crate[crate_of(f[1:])] += c

    sim = subprocess.run([TC / "sim-bfin/bin/bfin-elf-run", "--environment", "virtual",
                          counted],
                         capture_output=True, timeout=1800)
    if sim.returncode:
        sys.exit(f"simulator exit {sim.returncode}: {sim.stderr.decode(errors='replace')[-2000:]}")
    raw = sim.stdout
    nframes = 6  # Frame and Dirty marks
    for i in range(nframes):
        (OUT / f"frame{i:02}.x1rgb555").write_bytes(raw[i * FB:(i + 1) * FB])
    words = struct.unpack_from(f"<{(len(raw) - nframes * FB) // 4}I", raw, nframes * FB)
    status, stack, n = words[:3]
    marks = [words[3 + 3 * i: 6 + 3 * i] for i in range(n)]
    sf_totals = dict(zip(softfloat, words[3 + 3 * n:]))

    print(f"status {status}  sections: {sections}  (.data = rodata+data; .bss includes the 219 KiB framebuffer)")
    print(f"measured stack high-water {stack} B")
    text = Counter()
    for line in symbols.splitlines():
        f = line.split()
        if len(f) == 4 and f[2].lower() == "t":
            text[crate_of(f[3][1:] if f[3].startswith("__R") else f[3])] += int(f[1], 16)
    print(".text by defining crate:", text.most_common(12))
    print(f"soft-float helpers ({len(softfloat)}): {softfloat}")
    print(f"64/128-bit helpers: {wide}")
    print(f"soft-float call sites: {sum(soft_callers.values())} in {len(soft_callers)} functions; top:",
          [(short(k), v) for k, v in soft_callers.most_common(15)])
    print(f"panic call sites: {sum(sites.values())} in {len(sites)} functions; by crate: {by_crate.most_common()}")
    for f, c in sites.most_common():
        print(f"    {c:3}  {crate_of(f[1:]):20} {short(f)}")
    if args.lines and site_addrs:
        where = run(TC / "binutils-bfin/bin/bfin-elf-addr2line", "-i", "-e", elf, *site_addrs,
                    capture_output=True, text=True).stdout
        print("panic site lines (innermost first):")
        print("\n".join("    " + re.sub(r".*/(crates|library|spikes|src)/", r"\1/", l) for l in where.splitlines()))
    print("panic roots linked:", sorted(n for n in names if PANIC.search(n)))
    print(f"frames > 256 B ({len(big)}):", [(v, short(k)) for v, k in big[:15]])
    for (kind, cyc, sf), label in zip(marks, LABELS):
        print(f"{label:48} {cyc:10d} cycles  {cyc / MHZ / 1000:7.3f} ms @{MHZ}MHz  soft-float calls {sf}")
    print("soft-float calls per helper (whole run):", {k[3:]: v for k, v in sf_totals.items() if v})
    host = SPIKE / "target/frames"
    same = [(host / f"frame{i:02}.x1rgb555").read_bytes() == (OUT / f"frame{i:02}.x1rgb555").read_bytes()
            for i in range(nframes)]
    for i, s in enumerate(same):
        print(f"frame{i:02}.x1rgb555: Blackfin {'==' if s else '!='} host render")
    # Optional pin: frames saved from an earlier build (e.g. the spike's) in target/frames-base.
    base = SPIKE / "target/frames-base"
    vs_base = [(base / f"frame{i:02}.x1rgb555").read_bytes() == (OUT / f"frame{i:02}.x1rgb555").read_bytes()
               for i in range(nframes) if (base / f"frame{i:02}.x1rgb555").exists()]
    cyc = {label: c for (_, c, _), label in zip(marks, LABELS)}
    summary = {
        "opt": opt,
        "browse_full": cyc["browse: full frame"], "row_text": cyc["browse: 1 row text (render_dirty)"],
        "selection": cyc["browse: selection 0->1 (render_dirty, 2 rows)"],
        "player_full": cyc["player: full frame"], "digits": cyc["player: frames digits (render_dirty)"],
        "images": cyc.get("ref: 6 raw images 24x24 alone (render)"),
        "wave": cyc.get("ref: waveform alone, 400 column fill_rects (render)"),
        "a4_text": cyc.get("ref: 20 glyph-like A4 masks 24x24 on a 240x60 pane (render)"),
        "indexed_t0": cyc.get("ref: 400x90 indexed8, 70% transparent index 0 (render)"),
        "indexed_t255": cyc.get("ref: 400x90 indexed8, 70% transparent index 255 (render)"),
        "raw_fill": cyc["ref: raw fill 480x234 (no embedded-gui)"],
        "panel": cyc["ref: 1 full-screen panel (render)"],
        "per_glyph": (cyc["ref: 1 row label, 21 glyphs 9x18 (render)"] - cyc["ref: same label, no text (render)"]) // GLYPHS,
        "text": sections.get(".text", 0), "stack": stack,
        "ctx_bytes": next((int(f[1], 16) for f in map(str.split, symbols.splitlines())
                           if len(f) == 4 and "spike_run3GUI" in f[3]), None), "panic_sites": sum(sites.values()),
        "render_softfloat_calls": sum(sf for _, _, sf in marks), "identical": all(same),
        "same_as_base": all(vs_base) if vs_base else None,
    }
    print("SUMMARY", json.dumps(summary))
    gates = {"frames identical to host": summary["identical"], "0 reachable panic sites": not summary["panic_sites"],
             "0 soft-float calls while rendering": not summary["render_softfloat_calls"]}
    print("GATES", {k: "PASS" if v else "FAIL" for k, v in gates.items()})
    if not all(gates.values()):
        sys.exit(1)


if __name__ == "__main__":
    main()
