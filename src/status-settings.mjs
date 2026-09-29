import { readFileSync, statSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';

// Claude executes statusLine.command through a shell, including when paths have
// spaces, quotes, dollar signs, or backticks. Never interpolate them unquoted.
export function shellQuote(value) {
  return `'${String(value).replaceAll("'", "'\\''")}'`;
}

export function statusLineSettings() {
  const script = fileURLToPath(new URL('../bin/statusline.mjs', import.meta.url));
  return { type: 'command', command: `${shellQuote(process.execPath)} ${shellQuote(script)}`, padding: 0, refreshInterval: 1 };
}

function rebasePermissionRules(settings, sourceDirectory) {
  if (!settings.permissions || typeof settings.permissions !== 'object' || Array.isArray(settings.permissions)) return settings;
  // Read/Edit /path patterns anchor at dirname(--settings file), or cwd for
  // inline settings. Preserve that anchor when moving the settings to temp.
  // Absolute permission patterns use //path, unlike ordinary filesystem paths.
  // https://code.claude.com/docs/en/permissions#read-and-edit
  let anchor = sourceDirectory;
  if (process.platform === 'win32') anchor = anchor.replaceAll('\\', '/').replace(/^([a-z]):/i, (_, drive) => `/${drive.toLowerCase()}`);
  anchor = anchor.replace(/\/$/, '').replace(/[\\*?\[\]]/g, '\\$&');
  const permissions = { ...settings.permissions };
  for (const kind of ['allow', 'ask', 'deny']) {
    if (!Array.isArray(permissions[kind])) continue;
    permissions[kind] = permissions[kind].map(rule => {
      const match = typeof rule === 'string' && /^(Read|Edit)\((\/(?!\/)[\s\S]*)\)$/.exec(rule);
      return match ? `${match[1]}(/${anchor}${match[2]})` : rule;
    });
  }
  return { ...settings, permissions };
}

function checkSandboxPaths(settings) {
  // Sandbox paths use conventional /absolute and ~/home syntax, not the
  // Read/Edit // syntax. Documentation specifies source-dependent relative
  // paths but does not define the --settings file/inline anchor explicitly.
  // Keep original settings intact by declining the overlay for that case.
  const paths = [];
  for (const key of ['allowRead', 'allowWrite', 'denyRead', 'denyWrite']) {
    const values = settings.sandbox?.filesystem?.[key];
    if (Array.isArray(values)) paths.push(...values);
  }
  if (Array.isArray(settings.sandbox?.credentials?.files)) {
    paths.push(...settings.sandbox.credentials.files.map(entry => entry?.path));
  }
  if (paths.some(path => typeof path === 'string' && !path.startsWith('/') && !path.startsWith('~/'))) {
    throw new Error('Cannot safely relocate source-relative sandbox paths; keep the original Claude settings');
  }
}

export function addStatusLineSettings(args, directory) {
  const forwarded = [];
  let supplied;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--') { forwarded.push(...args.slice(i)); break; }
    if (args[i] === '--settings') {
      supplied = args[++i];
      if (!supplied || supplied.startsWith('--')) throw new Error('--settings requires a JSON object or settings file');
    } else if (args[i].startsWith('--settings=')) supplied = args[i].slice('--settings='.length);
    else forwarded.push(args[i]);
  }
  let settings = {};
  let sourceDirectory = process.cwd();
  if (supplied !== undefined) {
    try {
      let json = supplied;
      if (!supplied.trimStart().startsWith('{')) {
        const stat = statSync(supplied);
        if (!stat.isFile() || stat.size > 2 * 1024 * 1024) throw new Error();
        json = readFileSync(supplied, 'utf8');
        sourceDirectory = dirname(resolve(supplied));
      }
      settings = JSON.parse(json);
      if (!settings || typeof settings !== 'object' || Array.isArray(settings)) throw new Error();
    } catch {
      throw new Error('Could not read --settings as a JSON object; set AUTOROUTER_STATUSLINE=0 to pass it directly to Claude');
    }
  }
  // Use a private temporary file instead of putting user settings (which may
  // include sensitive env values) into process arguments. Saved files are intact.
  checkSandboxPaths(settings);
  const path = join(directory, 'claude-settings.json');
  writeFileSync(path, JSON.stringify({ ...rebasePermissionRules(settings, sourceDirectory), statusLine: statusLineSettings() }), { mode: 0o600 });
  return ['--settings', path, ...forwarded];
}
