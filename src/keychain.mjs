import { spawnSync } from 'node:child_process';

// macOS Keychain access through the system `security` tool. Secret values are
// passed on stdin to its interactive mode, never as process arguments, and are
// read back from stdout. Tool output can describe items, so failures report
// only a generic category.
const SECURITY = '/usr/bin/security';
const SERVICE = 'claude-autorouter';
const NOT_FOUND = 44;
const PRINTABLE = /^[\x20-\x7e]+$/;

function keychainError(message) {
  const error = new Error(message);
  error.code = 'AUTOROUTER_CONFIG_ERROR';
  return error;
}

// Quote for the `security -i` command parser, which accepts backslash escapes
// inside double quotes and reads one command per line. The secret is validated
// as printable single-line ASCII; the account and label (which can carry a
// user-chosen path) must contain no control or line-separator characters, since
// a newline would start another command.
const UNSAFE = /[\u0000-\u001f\u007f-\u009f\u2028\u2029]/;
const quote = value => {
  if (typeof value !== 'string' || UNSAFE.test(value)) throw keychainError('Keychain item names must not contain control characters.');
  return `"${value.replace(/[\\"]/g, '\\$&')}"`;
};

export function createKeychain({ run = spawnSync, platform = process.platform } = {}) {
  const available = platform === 'darwin';
  const call = (args, input) => {
    if (!available) throw keychainError('The macOS Keychain secret store is available only on macOS.');
    const result = run(SECURITY, args, {
      input, encoding: 'utf8', timeout: 15000, maxBuffer: 64 * 1024, stdio: ['pipe', 'pipe', 'pipe'],
    });
    if (result.error) throw keychainError('Could not run the macOS Keychain tool.');
    return result;
  };
  const read = account => {
    const result = call(['find-generic-password', '-s', SERVICE, '-a', account, '-w']);
    if (result.status === NOT_FOUND) return undefined;
    if (result.status !== 0) {
      throw keychainError('Could not read an AutoRouter secret from the macOS Keychain. Unlock the login keychain, or set the key in the environment.');
    }
    return String(result.stdout).replace(/\n$/, '');
  };
  return {
    available,
    read,
    write(account, value, label) {
      if (typeof value !== 'string' || !PRINTABLE.test(value)) {
        throw keychainError('Keychain secrets must be printable single-line ASCII.');
      }
      call(['-i'], `add-generic-password -U -s ${quote(SERVICE)} -a ${quote(account)} -l ${quote(label)} -w ${quote(value)}\n`);
      // Interactive mode exits successfully even when a command fails.
      if (read(account) !== value) throw keychainError('Could not save an AutoRouter secret to the macOS Keychain.');
    },
    remove(account) {
      const result = call(['delete-generic-password', '-s', SERVICE, '-a', account]);
      if (result.status !== 0 && result.status !== NOT_FOUND) {
        throw keychainError('Could not remove an AutoRouter secret from the macOS Keychain.');
      }
    },
  };
}
