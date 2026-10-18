// Verify the two synthetic same-prefix startup failure schedules in CI reports.
// Platform consumers retain their existing archive and rollback checks.
import assert from 'node:assert/strict';
import { posix as path } from 'node:path';
import { constants, openSync, closeSync, fstatSync, readSync } from 'node:fs';

const limits = [
  'two_synthetic_same_prefix_schedules_only',
  'no_connection_attempt_or_internal_close_order_claim',
  'no_partial_write_or_concurrent_owner_coverage',
  'no_process_tree_or_crash_guarantee',
];
const scheduleKeys = [
  'id', 'exit_code', 'stdout', 'stderr', 'gateway', 'gateway_closed',
  'zero_accepted_or_queued_backend_connections', 'live_resources',
  'config_history_bytes_and_metadata_unchanged',
  'history_directory_entries_unchanged', 'temporary_directory_empty',
];
const resourceKeys = [
  'status_path', 'settings_path', 'status', 'settings', 'status_sha256',
  'settings_sha256', 'status_copy_mode', 'settings_copy_mode',
  'original_resources_removed',
];
const keys = (value, expected) => {
  assert.ok(value !== null && typeof value === 'object' && !Array.isArray(value));
  assert.deepEqual(Object.keys(value).sort(), [...expected].sort());
};
const ownedPath = value => {
  assert.equal(typeof value, 'string');
  assert.ok(path.isAbsolute(value) && path.normalize(value) === value);
  assert.ok(!/[\u0000-\u001f\u007f]/u.test(value));
  return value;
};
const shellQuote = value => `'${value.replaceAll("'", "'\\''")}'`;

// target comes from the already-verified single host artifact, not a filename.
export function assertFailedStartup(rehearsal, target) {
  assert.equal(typeof target, 'string');
  assert.ok(/^[a-z0-9_]+(?:-[a-z0-9_]+)+$/u.test(target));
  const failure = rehearsal.failed_startup;
  keys(failure, ['schema_version', 'kind', 'schedules', 'limits']);
  assert.equal(failure.schema_version, 1);
  assert.equal(failure.kind, 'installed_failed_startup_recovery');
  assert.deepEqual(failure.limits, limits);
  assert.ok(Array.isArray(failure.schedules));
  assert.deepEqual(failure.schedules.map(row => row.id), [
    'fake_claude_nonzero', 'missing_claude',
  ]);
  for (const [index, row] of failure.schedules.entries()) {
    keys(row, scheduleKeys);
    const fake = index === 0;
    assert.equal(row.exit_code, fake ? 23 : 1);
    const endpoint = /^http:\/\/127\.0\.0\.1:([1-9][0-9]{0,4})$/u.exec(row.gateway);
    assert.ok(endpoint && Number(endpoint[1]) <= 65535);
    const startup = `AutoRouter listening on ${row.gateway} (api-key authentication)\n`
      + 'AutoRouter uses Haiku-compatible requests with client thinking disabled; Jev selects the upstream model.\n';
    assert.equal(row.stderr, startup + (fake ? ''
      : 'Could not launch Claude Code. Ensure `claude` is installed and on PATH.\n'));
    for (const field of [
      'gateway_closed', 'zero_accepted_or_queued_backend_connections',
      'config_history_bytes_and_metadata_unchanged',
      'history_directory_entries_unchanged', 'temporary_directory_empty',
    ]) assert.equal(row[field], true);
    if (!fake) {
      assert.equal(row.stdout, '');
      assert.equal(row.live_resources, null);
      continue;
    }
    const live = row.live_resources;
    keys(live, resourceKeys);
    const statusPath = ownedPath(live.status_path);
    const settingsPath = ownedPath(live.settings_path);
    const directory = path.dirname(statusPath);
    const temporary = path.dirname(directory);
    assert.ok(path.basename(directory).startsWith('autorouter-status-'));
    assert.equal(path.basename(temporary), 'failed-startup-fake_claude_nonzero-temp');
    assert.equal(path.basename(statusPath), 'state.json');
    assert.equal(settingsPath, path.join(directory, 'claude-settings.json'));
    assert.equal(row.stdout, `${row.gateway}\n${statusPath}\n${settingsPath}\n`);
    keys(live.status, ['version', 'pid', 'heartbeat_at', 'sessions', 'savings']);
    assert.equal(live.status.version, 1);
    assert.ok(Number.isInteger(live.status.pid) && live.status.pid > 0
      && live.status.pid <= 0xffffffff);
    assert.ok(Number.isInteger(live.status.heartbeat_at) && live.status.heartbeat_at > 0);
    assert.deepEqual(live.status.sessions, {});
    assert.deepEqual(live.status.savings, {});
    const installed = path.join(path.dirname(temporary), "install prefix ' $ with spaces",
      'lib/node_modules/claude-autorouter/native', target, 'claude-autorouter');
    assert.deepEqual(live.settings, { statusLine: {
      type: 'command', command: `${shellQuote(installed)} statusline`,
      padding: 0, refreshInterval: 1,
    } });
    for (const field of ['status_sha256', 'settings_sha256']) {
      assert.equal(typeof live[field], 'string');
      assert.match(live[field], /^[0-9a-f]{64}$/u);
    }
    assert.equal(live.status_copy_mode, 0o600);
    assert.equal(live.settings_copy_mode, 0o600);
    assert.equal(live.original_resources_removed, true);
  }
}

// Synthetic parser controls only: no process, socket or installed startup.
const parserControls = {
  "target": "x86_64-unknown-linux-gnu",
  "positive": {
    "failed_startup": {
      "schema_version": 1,
      "kind": "installed_failed_startup_recovery",
      "schedules": [
        {
          "id": "fake_claude_nonzero",
          "exit_code": 23,
          "stdout": "http://127.0.0.1:42317\n/synthetic/private/failed-startup-fake_claude_nonzero-temp/autorouter-status-fixture/state.json\n/synthetic/private/failed-startup-fake_claude_nonzero-temp/autorouter-status-fixture/claude-settings.json\n",
          "stderr": "AutoRouter listening on http://127.0.0.1:42317 (api-key authentication)\nAutoRouter uses Haiku-compatible requests with client thinking disabled; Jev selects the upstream model.\n",
          "gateway": "http://127.0.0.1:42317",
          "gateway_closed": true,
          "zero_accepted_or_queued_backend_connections": true,
          "live_resources": {
            "status_path": "/synthetic/private/failed-startup-fake_claude_nonzero-temp/autorouter-status-fixture/state.json",
            "settings_path": "/synthetic/private/failed-startup-fake_claude_nonzero-temp/autorouter-status-fixture/claude-settings.json",
            "status": {
              "version": 1,
              "pid": 111111,
              "heartbeat_at": 1800000000000,
              "sessions": {},
              "savings": {}
            },
            "settings": {
              "statusLine": {
                "type": "command",
                "command": "'/synthetic/private/install prefix '\\'' $ with spaces/lib/node_modules/claude-autorouter/native/x86_64-unknown-linux-gnu/claude-autorouter' statusline",
                "padding": 0,
                "refreshInterval": 1
              }
            },
            "status_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "settings_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "status_copy_mode": 384,
            "settings_copy_mode": 384,
            "original_resources_removed": true
          },
          "config_history_bytes_and_metadata_unchanged": true,
          "history_directory_entries_unchanged": true,
          "temporary_directory_empty": true
        },
        {
          "id": "missing_claude",
          "exit_code": 1,
          "stdout": "",
          "stderr": "AutoRouter listening on http://127.0.0.1:42318 (api-key authentication)\nAutoRouter uses Haiku-compatible requests with client thinking disabled; Jev selects the upstream model.\nCould not launch Claude Code. Ensure `claude` is installed and on PATH.\n",
          "gateway": "http://127.0.0.1:42318",
          "gateway_closed": true,
          "zero_accepted_or_queued_backend_connections": true,
          "live_resources": null,
          "config_history_bytes_and_metadata_unchanged": true,
          "history_directory_entries_unchanged": true,
          "temporary_directory_empty": true
        }
      ],
      "limits": [
        "two_synthetic_same_prefix_schedules_only",
        "no_connection_attempt_or_internal_close_order_claim",
        "no_partial_write_or_concurrent_owner_coverage",
        "no_process_tree_or_crash_guarantee"
      ]
    }
  },
  "negatives": [
    {
      "name": "absent_object",
      "op": "delete",
      "path": [
        "failed_startup"
      ]
    },
    {
      "name": "missing_schedule",
      "op": "pop",
      "path": [
        "failed_startup",
        "schedules"
      ]
    },
    {
      "name": "wrong_order",
      "op": "reverse",
      "path": [
        "failed_startup",
        "schedules"
      ]
    },
    {
      "name": "fake_wrong_exit",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "exit_code"
      ],
      "value": 0
    },
    {
      "name": "missing_wrong_exit",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        1,
        "exit_code"
      ],
      "value": 0
    },
    {
      "name": "gateway_closed",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "gateway_closed"
      ],
      "value": false
    },
    {
      "name": "zero_accepted_or_queued_backend_connections",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "zero_accepted_or_queued_backend_connections"
      ],
      "value": false
    },
    {
      "name": "config_history_bytes_and_metadata_unchanged",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "config_history_bytes_and_metadata_unchanged"
      ],
      "value": false
    },
    {
      "name": "history_directory_entries_unchanged",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "history_directory_entries_unchanged"
      ],
      "value": false
    },
    {
      "name": "temporary_directory_empty",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "temporary_directory_empty"
      ],
      "value": false
    },
    {
      "name": "missing_case_live_resources",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        1,
        "live_resources"
      ],
      "value": {}
    },
    {
      "name": "no_positive_live_copy",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "live_resources"
      ],
      "value": null
    },
    {
      "name": "nonempty_sessions",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "live_resources",
        "status",
        "sessions"
      ],
      "value": {
        "foreign": {}
      }
    },
    {
      "name": "wrong_installed_command",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "live_resources",
        "settings",
        "statusLine",
        "command"
      ],
      "value": "'/foreign/executable' statusline"
    },
    {
      "name": "public_copy_mode",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "live_resources",
        "settings_copy_mode"
      ],
      "value": 420
    },
    {
      "name": "retained_original_resource",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "live_resources",
        "original_resources_removed"
      ],
      "value": false
    },
    {
      "name": "truncated_stdout",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "stdout"
      ],
      "value": ""
    },
    {
      "name": "extra_diagnostic",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        1,
        "stderr"
      ],
      "value": "AutoRouter listening on http://127.0.0.1:42318 (api-key authentication)\nAutoRouter uses Haiku-compatible requests with client thinking disabled; Jev selects the upstream model.\nCould not launch Claude Code. Ensure `claude` is installed and on PATH.\nunexpected\n"
    },
    {
      "name": "foreign_gateway",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "gateway"
      ],
      "value": "http://192.0.2.1:42317"
    },
    {
      "name": "boolean_pid",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "live_resources",
        "status",
        "pid"
      ],
      "value": true
    },
    {
      "name": "malformed_copy_hash",
      "op": "set",
      "path": [
        "failed_startup",
        "schedules",
        0,
        "live_resources",
        "status_sha256"
      ],
      "value": "bad"
    }
  ]
};

function selfTest() {
  assertFailedStartup(parserControls.positive, parserControls.target);
  for (const mutation of parserControls.negatives) {
    const changed = structuredClone(parserControls.positive);
    let parent = changed;
    for (const key of mutation.path.slice(0, -1)) parent = parent[key];
    const key = mutation.path.at(-1);
    if (mutation.op === 'delete') delete parent[key];
    else if (mutation.op === 'pop') parent[key].pop();
    else if (mutation.op === 'reverse') parent[key].reverse();
    else parent[key] = structuredClone(mutation.value);
    assert.throws(() => assertFailedStartup(changed, parserControls.target), undefined, mutation.name);
  }
  return { passed: true, positive_controls: 1, negative_controls: parserControls.negatives.length };
}

function readReport(filename, limit) {
  const fd = openSync(filename, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const before = fstatSync(fd, { bigint: true });
    assert.ok(before.isFile() && before.size <= BigInt(limit));
    const chunks = [];
    let length = 0;
    for (;;) {
      const chunk = Buffer.alloc(Math.min(65536, limit - length + 1));
      const count = readSync(fd, chunk, 0, chunk.length, null);
      if (count === 0) break;
      length += count;
      assert.ok(length <= limit);
      chunks.push(chunk.subarray(0, count));
    }
    const after = fstatSync(fd, { bigint: true });
    for (const field of ['dev', 'ino', 'size', 'mtimeNs', 'ctimeNs']) {
      assert.equal(after[field], before[field]);
    }
    assert.equal(BigInt(length), before.size);
    return JSON.parse(Buffer.concat(chunks, length).toString('utf8'));
  } finally { closeSync(fd); }
}

try {
  const args = process.argv.slice(2);
  let result;
  if (args.length === 1 && args[0] === '--self-test') result = selfTest();
  else {
    assert.equal(args.length, 2);
    const rehearsal = readReport(args[0], 64 * 1024);
    const packed = readReport(args[1], 1024 * 1024);
    assert.equal(rehearsal.passed, true);
    assert.equal(packed.verified, true);
    assert.equal(packed.kind, 'native_npm_feasibility');
    assert.match(packed.sha256, /^[0-9a-f]{64}$/u);
    assert.equal(rehearsal.native_archive_sha256, packed.sha256);
    assert.ok(Array.isArray(packed.artifacts));
    assert.equal(packed.artifacts.length, 1);
    assertFailedStartup(rehearsal, packed.artifacts[0].target);
    result = { passed: true, schedules: 2 };
  }
  process.stdout.write(`${JSON.stringify(result)}\n`);
} catch {
  process.stderr.write('Upgrade/rollback failed-startup evidence rejected\n');
  process.exitCode = 1;
}
