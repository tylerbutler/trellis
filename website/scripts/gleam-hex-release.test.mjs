import assert from "node:assert/strict";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";

const workflow = readFileSync(new URL("../../examples/gleam-hex-release.yml", import.meta.url), "utf8");
const scripts = Object.fromEntries([...workflow.matchAll(
  /            \/\/ script: (\w+)\n([\s\S]*?)            \/\/ end-script/g,
)].map(([, name, source]) => [name, source.replace(/^            /gm, "")]));
const installation = workflow.match(/          # script: install\n([\s\S]*?)          # end-script/)[1]
  .replace(/^          /gm, "");
const repo = { owner: "owner", repo: "workspace" };
const origin = { full_name: "owner/workspace" };
const release_pr = (overrides = {}) => ({
  number: 12, merged: true, merged_at: "2026-11-01T00:00:00Z", merge_commit_sha: "approved",
  head: { ref: "release/pending", sha: "candidate", repo: origin },
  base: { ref: "main", repo: origin }, ...overrides,
});
const normalize = value => JSON.parse(JSON.stringify(value));

function harness(options = {}) {
  const events = [];
  const outputs = {};
  let head = options.head ?? "main_sha";
  const statuses = { ...options.statuses };
  const closed = options.closed ?? [];
  const open = options.open ?? [];
  const env = { RELEASE_SHA: "approved", ...options.env };
  const context = {
    repo, eventName: options.event ?? "push", ref: "refs/heads/main",
    payload: { pull_request: options.pr ?? release_pr() },
    runId: 42, serverUrl: "https://github.com", ...options.context,
  };
  const rest = {
    pulls: { list: "pulls.list", get: async () => ({ data: options.live_pr ?? context.payload.pull_request }) },
    repos: {
      listCommitStatusesForRef: "statuses.list", listCommits: "commits.list",
      createCommitStatus: async status => {
        events.push(["status", status.context, status.state, status.sha]);
        if (options.fail_status === status.state) throw new Error("Status write failed");
        statuses[status.sha] = [status, ...(statuses[status.sha] ?? [])];
      },
    },
  };
  const paginate = async (method, args) => {
    events.push(["api", method, normalize(args)]);
    if (method === "statuses.list") return statuses[args.ref] ?? [];
    if (method === "pulls.list") return args.state === "closed" ? closed : open;
    throw new Error(`Unexpected API: ${method}`);
  };
  paginate.iterator = async function* () {
    for (const commits of options.commit_pages ?? [options.commits ?? ["main_sha"]]) {
      yield { data: commits.map(sha => ({ sha })) };
    }
  };
  const exec = {
    getExecOutput: async (command, args) => {
      events.push(["exec", command, normalize(args)]);
      if (command === "git") return { stdout: `${head}\n` };
      assert.deepEqual(normalize(args), ["list", "--releasable", "--json"]);
      return { stdout: JSON.stringify({ packages: [
        { version: head === "candidate" ? options.pending_version ?? "1.1.0" : options.version ?? "1.0.0" },
      ] }) };
    },
    exec: async (command, args) => {
      events.push(["exec", command, normalize(args)]);
      if (options.fail?.(command, normalize(args))) throw new Error("Command failed");
      if (command === "git" && args[0] === "checkout") head = args.at(-1);
    },
  };
  return {
    events, outputs, statuses,
    async run(name) {
      assert.ok(scripts[name], `Missing inline script: ${name}`);
      const runtime = runInNewContext(
        `(async function(github, context, core, glob, io, exec, require, __original_require__) {${scripts[name]}})`,
        { process: { env } },
      );
      const args = {
        github: { rest, paginate }, context, process: { env },
        require: () => { throw new Error("Use the injected github-script runtime modules"); },
        core: {
          info: message => events.push(["info", message]),
          setOutput: (name, value) => { outputs[name] = value; },
        },
      };
      return runtime(args.github, context, args.core, {}, {}, exec, args.require, args.require);
    },
  };
}
const release_status = state => ({ context: "trellis/release", state });
const reservation = { context: "trellis/release_request", state: "pending" };
const release_commands = h => h.events.filter(e => e[0] === "exec" && e[1] === "trellis" && e[2][0] === "release");

test("workflow queues preparation and publication without cancellation", () => {
  assert.match(workflow, /concurrency:\n  group: trellis-release\n  queue: max\n  cancel-in-progress: false/);
  assert.match(workflow, /ref: \$\{\{ steps.select.outputs.publish_sha \}\}/);
  assert.match(workflow, /name: Check out current main[\s\S]*?ref: main/);
  assert.match(workflow, /steps.publish.outputs.complete == 'true'/);
  assert.match(workflow, /statuses: write/);
  assert.match(workflow, /token: \$\{\{ steps.app.outputs.token \}\}/);
  assert.doesNotMatch(workflow, /run:.*\$\{\{ inputs\./);
});

test("only same-repository merged release PRs on main select the immutable merge SHA", async () => {
  const h = harness({ event: "pull_request" });
  await h.run("select");
  assert.equal(h.outputs.publish_sha, "approved");
  for (const pr of [
    release_pr({ merged: false }),
    release_pr({ head: { ref: "feature", repo: origin } }),
    release_pr({ head: { ref: "release/pending", repo: { full_name: "fork/workspace" } } }),
    release_pr({ base: { ref: "other", repo: origin } }),
    release_pr({ base: { ref: "main", repo: { full_name: "other/workspace" } } }),
    release_pr({ merge_commit_sha: null }),
  ]) {
    const rejected = harness({ event: "pull_request", pr });
    await rejected.run("select");
    assert.deepEqual(rejected.outputs, {});
  }
  const changed = harness({ event: "pull_request", live_pr: release_pr({ merge_commit_sha: "different" }) });
  await changed.run("select");
  assert.deepEqual(changed.outputs, {});
});

test("completed publication reruns skip publishing but retry gated current-main preparation", async () => {
  const h = harness({
    event: "pull_request", closed: [release_pr()],
    statuses: { approved: [release_status("success")] },
    commits: ["main_sha", "approved"], version: "1.1.0",
  });
  await h.run("select");
  assert.deepEqual(h.outputs, { prepare: "true" });
  await h.run("prepare");
  assert.equal(release_commands(h).length, 1);
  assert.equal(h.events.filter(e => e[0] === "exec" && ["publish", "tag"].includes(e[2][0])).length, 0);
});

test("stable resume recovers a preparation failure after successful publication", async () => {
  const first = harness({
    event: "pull_request", head: "approved", closed: [release_pr()],
    commits: ["main_sha", "approved"],
    fail: (cmd, args) => cmd === "trellis" && args[0] === "release",
  });
  await first.run("publish");
  await assert.rejects(first.run("prepare"), /Command failed/);
  const retry = harness({
    event: "pull_request", head: "fresh_main", closed: [release_pr()],
    statuses: first.statuses, commits: ["fresh_main", "approved"],
  });
  await retry.run("select");
  assert.deepEqual(retry.outputs, { prepare: "true" });
  await retry.run("prepare");
  assert.equal(release_commands(retry).length, 1);
  assert.equal(retry.events.filter(e => e[0] === "exec" && ["publish", "tag"].includes(e[2][0])).length, 0);
});

test("installation executes the reviewed source or pinned release path without fallback", () => {
  const directory = fileURLToPath(new URL(`../../.workflow-install-check-${process.pid}`, import.meta.url));
  try {
    mkdirSync(join(directory, "bin"), { recursive: true });
    for (const tool of ["cargo", "rustup", "curl"]) {
      writeFileSync(join(directory, "bin", tool), `#!/bin/sh
printf '%s %s\\n' '${tool}' "$*" >> "$CALL_LOG"
${tool === "cargo" ? 'printf "target %s\\n" "$CARGO_TARGET_DIR" >> "$CALL_LOG"\nexit "${FAIL_CARGO:-0}"' : ""}
${tool === "curl" ? `printf '%s\\n' 'printf "%s\\n" binary_installer >> "$CALL_LOG"'` : ""}
`, { mode: 0o755 });
    }
    for (const [name, version, source, fail_cargo] of [
      ["source", "", true, "0"],
      ["release", "9.9.9", true, "0"],
      ["missing_source", "", false, "0"],
      ["failed_source", "", true, "1"],
    ]) {
      const cwd = join(directory, name);
      mkdirSync(cwd);
      if (source) {
        mkdirSync(join(cwd, ".github/trellis"), { recursive: true });
        writeFileSync(join(cwd, ".github/trellis/Cargo.toml"), "[package]\n");
      }
      const call_log = join(cwd, "calls");
      const github_path = join(cwd, "github_path");
      writeFileSync(call_log, "");
      const result = spawnSync("bash", ["-c", installation], {
        cwd, encoding: "utf8",
        env: {
          ...process.env, PATH: `${join(directory, "bin")}:${process.env.PATH}`,
          TRELLIS_VERSION: version, RUNNER_TEMP: cwd, GITHUB_PATH: github_path,
          CALL_LOG: call_log, FAIL_CARGO: fail_cargo,
        },
      });
      const calls = readFileSync(call_log, "utf8");
      if (name === "source") {
        assert.equal(result.status, 0, result.stderr);
        assert.match(calls, /rustup toolchain install 1\.94\.1 --profile minimal/);
        assert.ok(calls.includes(`cargo +1.94.1 install --locked --path .github/trellis --root ${cwd}/trellis`));
        assert.ok(calls.includes(`target ${cwd}/trellis-target`));
        assert.equal(readFileSync(github_path, "utf8").trim(), `${cwd}/trellis/bin`);
        assert.doesNotMatch(calls, /curl/);
      } else if (name === "release") {
        assert.equal(result.status, 0, result.stderr);
        assert.match(calls, /releases\/download\/v9\.9\.9\/trellis-gleam-installer\.sh/);
        assert.match(calls, /binary_installer/);
        assert.doesNotMatch(calls, /cargo|rustup/);
      } else {
        assert.notEqual(result.status, 0);
        assert.doesNotMatch(calls, /curl|binary_installer/);
      }
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("dispatch requires main", async () => {
  const h = harness({ event: "workflow_dispatch", context: { ref: "refs/heads/feature" } });
  await assert.rejects(h.run("select"), /on main/);
});

test("automatic stable preparation creates and refreshes ordinary stable PRs", async () => {
  for (const open of [[], [release_pr({ merged: false })]]) {
    const h = harness({ open, version: "1.0.0+build-with-hyphen" });
    await h.run("prepare");
    assert.deepEqual(release_commands(h).map(e => e[2]), [
      ["release", "pr", "--base", "main", "--branch", "release/pending"],
    ]);
  }
});

test("automatic preparation pauses for pending candidates and prerelease main", async () => {
  for (const options of [
    { version: "1.1.0-rc.1" },
    { open: [release_pr({ merged: false })], pending_version: "1.1.0-rc.1" },
  ]) {
    const h = harness(options);
    await h.run("prepare");
    assert.deepEqual(release_commands(h), []);
  }
});

test("manual ownership blocks stable promotion refresh and survives unrelated main pushes", async () => {
  for (const open of [[], [release_pr({ merged: false })]]) {
    const h = harness({
      open, commits: ["new_main", "reserved_base", "older"],
      statuses: { reserved_base: [reservation] },
    });
    await h.run("prepare");
    assert.deepEqual(release_commands(h), []);
  }
});

test("manual ownership scanning covers paginated history", async () => {
  const h = harness({
    commit_pages: [["main_sha"], ["reserved_base"]],
    statuses: { reserved_base: [reservation] },
  });
  await h.run("prepare");
  assert.deepEqual(release_commands(h), []);
});

test("manual reservation is written before command and persists across the PR-creation failure window", async () => {
  const h = harness({ event: "workflow_dispatch", fail: (cmd, args) => cmd === "trellis" && args[0] === "release" });
  await assert.rejects(h.run("prepare"), /Command failed/);
  const status_index = h.events.findIndex(e => e[0] === "status");
  const command_index = h.events.findIndex(e => e[0] === "exec" && e[2][0] === "release");
  assert.ok(status_index < command_index);
  assert.equal(h.statuses.main_sha[0].state, "pending");
  const automatic = harness({ statuses: h.statuses });
  await automatic.run("prepare");
  assert.deepEqual(release_commands(automatic), []);
});

test("manual inputs use repeatable argument arrays, including literal shell syntax", async () => {
  const h = harness({
    event: "workflow_dispatch", version: "1.1.0-rc.1",
    open: [release_pr({ merged: false })],
    env: { INPUT_PRE: " none ", INPUT_SET: "core=1.1.0\ncli=2.0.0\r\n", INPUT_BUMP: "cli=minor\n$(touch bad)" },
  });
  await h.run("prepare");
  assert.deepEqual(release_commands(h)[0][2], [
    "release", "pr", "--base", "main", "--branch", "release/pending",
    "--pre", "none", "--set", "core=1.1.0", "--set", "cli=2.0.0",
    "--bump", "cli=minor", "--bump", "$(touch bad)",
  ]);
  assert.equal(h.statuses.main_sha[0].state, "pending");
});

test("a successful manual no-op releases its reservation", async () => {
  const h = harness({ event: "workflow_dispatch" });
  await h.run("prepare");
  assert.deepEqual(h.events.filter(e => e[0] === "status").map(e => e[2]), ["pending", "success"]);
});

test("latest merged release requires durable success, including before publication begins", async () => {
  for (const state of [undefined, "pending", "failure", "error"]) {
    for (const event of ["push", "workflow_dispatch"]) {
      const h = harness({
        event, closed: [release_pr()],
        statuses: { approved: state ? [release_status(state)] : [] },
      });
      await h.run("prepare");
      assert.deepEqual(release_commands(h), []);
    }
  }
});

test("the newest status controls recovery rather than any historical success", async () => {
  const h = harness({
    closed: [release_pr()],
    statuses: { approved: [release_status("failure"), release_status("success")] },
  });
  await h.run("prepare");
  assert.deepEqual(release_commands(h), []);
  const old_run = harness({
    event: "pull_request",
    statuses: { approved: [release_status("pending"), release_status("success")] },
  });
  await old_run.run("select");
  assert.equal(old_run.outputs.publish_sha, "approved");
});

test("latest release is chosen by merge time, not update order, and ignores untrusted PRs", async () => {
  const h = harness({
    closed: [
      release_pr({ merge_commit_sha: "old", merged_at: "2026-10-01T00:00:00Z" }),
      release_pr({ head: { ref: "feature", repo: origin }, merged_at: "2026-11-02T00:00:00Z" }),
      release_pr(),
    ],
    statuses: { approved: [release_status("success")], old: [release_status("failure")] },
  });
  await h.run("prepare");
  assert.equal(release_commands(h).length, 1);
});

test("publication pins checkout, reports pending, publishes before tags, then reports success", async () => {
  const h = harness({ head: "approved" });
  await h.run("publish");
  assert.deepEqual(h.events.filter(e => e[0] !== "api"), [
    ["exec", "git", ["rev-parse", "HEAD"]],
    ["status", "trellis/release", "pending", "approved"],
    ["exec", "trellis", ["publish", "--all-untagged"]],
    ["exec", "trellis", ["tag", "create", "--push", "--github-release"]],
    ["status", "trellis/release", "success", "approved"],
  ]);
  assert.equal(h.outputs.complete, "true");
  const wrong_checkout = harness({ head: "later_release" });
  await assert.rejects(wrong_checkout.run("publish"), /checkout changed/);
  assert.equal(wrong_checkout.events.filter(e => e[0] === "status").length, 0);
});

test("failed publication never tags and failed tagging never reports successful completion", async () => {
  for (const failed_step of ["publish", "tag"]) {
    const h = harness({ head: "approved", fail: (cmd, args) => cmd === "trellis" && args[0] === failed_step });
    await assert.rejects(h.run("publish"), /Command failed/);
    assert.deepEqual(h.events.filter(e => e[0] === "status").map(e => e[2]), ["pending", "failure"]);
    assert.deepEqual(h.outputs, {});
    if (failed_step === "publish") {
      assert.equal(h.events.filter(e => e[0] === "exec" && e[2][0] === "tag").length, 0);
    }
    const retry = harness({ head: "approved", statuses: h.statuses });
    await retry.run("publish");
    assert.equal(retry.statuses.approved[0].state, "success");
  }
});

test("status write errors cannot produce a successful completion fallback", async () => {
  for (const fail_status of ["pending", "success", "failure"]) {
    const h = harness({
      head: "approved", fail_status,
      fail: (cmd, args) => fail_status === "failure" && cmd === "trellis" && args[0] === "publish",
    });
    await assert.rejects(h.run("publish"), /Status write failed/);
    assert.deepEqual(h.outputs, {});
    assert.notEqual(h.statuses.approved?.[0]?.state, "success");
  }
});

test("successful candidate publication stays paused; promotion success resumes from current main", async () => {
  for (const version of ["1.1.0-rc.1", "1.1.0"]) {
    const h = harness({
      head: "approved", version, closed: [release_pr()],
      commits: ["main_sha", "approved", "old_manual_base"],
      statuses: { old_manual_base: [reservation] },
    });
    await h.run("publish");
    await h.run("prepare");
    assert.equal(release_commands(h).length, version.includes("-") ? 0 : 1);
  }
});
