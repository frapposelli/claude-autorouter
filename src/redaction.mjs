// @ts-check
// Pattern-based redaction for evaluator excerpts. Classification needs the
// shape of a task, not credentials or personal identifiers, so recognizable
// values are replaced before any excerpt leaves the inference path. This is a
// best-effort filter: unrecognized formats can remain, and code that merely
// resembles an assignment can be over-redacted. Every quantifier is bounded or
// excludes its delimiter, so scanning stays linear in the input length.
//
// This is deliberately not a dependency. Scanner libraries such as secretlint
// detect provider token formats only; they miss the assignment, URL and command
// forms below, and would add dozens of packages to a CLI that ships none.

const marker = kind => `[REDACTED:${kind}]`;

function ibanValid(value) {
  const compact = value.replace(/ /g, '');
  if (compact.length < 15 || compact.length > 34) return false;
  const rearranged = compact.slice(4) + compact.slice(0, 4);
  let remainder = 0;
  for (const character of rearranged) {
    const digits = /[0-9]/.test(character) ? character : String(character.charCodeAt(0) - 55);
    for (const digit of digits) remainder = (remainder * 10 + Number(digit)) % 97;
  }
  return remainder === 1;
}

function luhnValid(value) {
  const digits = value.replace(/[ -]/g, '');
  if (digits.length < 13 || digits.length > 19) return false;
  let sum = 0;
  for (let i = 0; i < digits.length; i++) {
    let digit = Number(digits[digits.length - 1 - i]);
    if (i % 2 === 1) { digit *= 2; if (digit > 9) digit -= 9; }
    sum += digit;
  }
  return sum % 10 === 0;
}

const CF_ODD = [1, 0, 5, 7, 9, 13, 15, 17, 19, 21, 2, 4, 18, 20, 11, 3, 6, 8, 12, 14, 16, 10, 22, 25, 24, 23];
function codiceFiscaleValid(value) {
  const text = value.toUpperCase();
  if (text.length !== 16) return false;
  let sum = 0;
  for (let i = 0; i < 15; i++) {
    const code = text.charCodeAt(i);
    const index = code >= 65 ? code - 65 : code - 48;
    if (index < 0 || index > 25) return false;
    sum += i % 2 === 0 ? CF_ODD[index] : index;
  }
  return text.charCodeAt(15) === 65 + (sum % 26);
}

function phoneValid(value) {
  const digits = value.replace(/[^0-9]/g, '').length;
  return digits >= 8 && digits <= 15;
}

const base64 = code => (code >= 48 && code <= 57) || (code >= 65 && code <= 90) || (code >= 97 && code <= 122)
  || code === 43 || code === 47 || code === 61;

// Key material whose BEGIN line was cut off before the excerpt: walk back from
// each END line over whole base64 lines. A regex line repetition would
// backtrack quadratically on a long base64 run that is not followed by END.
function redactKeyTails(text) {
  const end = /-----END [A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----/g;
  let output = '', copied = 0;
  for (let match; (match = end.exec(text));) {
    let start = match.index;
    while (start > copied && text[start - 1] === '\n') {
      let lineEnd = start - 1;
      if (lineEnd > copied && text[lineEnd - 1] === '\r') lineEnd--;
      let lineStart = lineEnd;
      while (lineStart > copied && base64(text.charCodeAt(lineStart - 1))) lineStart--;
      if (lineEnd - lineStart < 16 || (lineStart > copied && text[lineStart - 1] !== '\n')) break;
      start = lineStart;
    }
    if (start === match.index) continue;
    output += text.slice(copied, start) + marker('private_key');
    copied = end.lastIndex;
  }
  return copied ? output + text.slice(copied) : text;
}

// Setting names that hold a secret. "pass", "auth" and "key" are too common in
// ordinary names (tests_pass, AUTH_MODE, primary_key), so they are matched only
// as upper-case environment names or as the whole word, further below.
const SECRET_NAME = '(?:passw(?:or)?d|pwd|passphrase|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|signing[_-]?key|encryption[_-]?key|credentials?)';
// Characters that may follow an unquoted value. A comma or semicolon ends it
// only before whitespace, so a password such as a;b is not cut after one letter.
const VALUE = '(?:[^\\s"\'&,;]|[,;](?=[^\\s"\'&]))';
const SEPARATOR = '(["\']?[ \\t]{0,8}[:=][ \\t]{0,8})';
const FLAG_NAME = '(?:password|passwd|pwd|passphrase|pass|token|secret|api-?key|access-?key|private-?key|client-?secret|auth-?token|auth)';

/** @type {ReadonlyArray<readonly [RegExp, string | ((...match: string[]) => string)] | ((text: string) => string)>} */
const RULES = Object.freeze([
  // A block cut off by excerpting is still redacted through the end of text.
  [/-----BEGIN [A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----[\s\S]*?(?:-----END [A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----|$)/g, marker('private_key')],
  redactKeyTails,
  // The userinfo may have an empty user (redis://:pw@host) and a password that
  // contains "@": the match runs to the last "@" before the host.
  [/\b([a-z][a-z0-9+.-]{1,20}:\/\/)[^\s:@/]{0,256}:[^\s/]{1,256}@/gi, (_, scheme) => `${scheme}${marker('credentials')}@`],
  // Provider token formats with distinctive prefixes.
  [/\bsk-[A-Za-z0-9_-]{20,}/g, marker('secret')],
  [/\b[rs]k_(?:live|test)_[A-Za-z0-9]{16,}/g, marker('secret')],
  [/\bwhsec_[A-Za-z0-9]{16,}/g, marker('secret')],
  [/\b(?:AKIA|ASIA|ABIA|ACCA)[A-Z0-9]{16}\b/g, marker('secret')],
  [/\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})/g, marker('secret')],
  [/\bglpat-[A-Za-z0-9_-]{20,}/g, marker('secret')],
  [/\bxox[abposr]-[A-Za-z0-9-]{10,}/g, marker('secret')],
  [/https:\/\/hooks\.slack\.com\/services\/[A-Za-z0-9/]{8,}/g, marker('secret')],
  [/\bAIza[0-9A-Za-z_-]{35}/g, marker('secret')],
  [/\bya29\.[A-Za-z0-9_-]{20,}/g, marker('secret')],
  [/\bnpm_[A-Za-z0-9]{36}/g, marker('secret')],
  [/\bpypi-[A-Za-z0-9_-]{50,}/g, marker('secret')],
  [/\bSG\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}/g, marker('secret')],
  [/\bshp(?:at|ca|pa|ss)_[a-f0-9]{32}\b/g, marker('secret')],
  [/\bhf_[A-Za-z0-9]{30,}/g, marker('secret')],
  [/\bdo[por]_v1_[a-f0-9]{64}\b/g, marker('secret')],
  [/\blin_api_[A-Za-z0-9]{30,}/g, marker('secret')],
  [/\bntn_[A-Za-z0-9]{30,}/g, marker('secret')],
  [/\bdapi[a-f0-9]{32}\b/g, marker('secret')],
  [/\bATATT3[A-Za-z0-9_=-]{20,}/g, marker('secret')],
  [/\bkey-[0-9a-f]{32}\b/g, marker('secret')],
  [/\bSK[0-9a-f]{32}\b/g, marker('secret')],
  [/\b[0-9]{8,10}:[A-Za-z0-9_-]{35}\b/g, marker('secret')],
  [/\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/g, marker('secret')],
  [/\b((?:AccountKey|SharedAccessKey|SharedAccessSignature)=)[^;\s"']{8,}/g, (_, prefix) => `${prefix}${marker('secret')}`],
  [/\b((?:proxy-)?authorization["']?[ \t]{0,8}[:=][ \t]{0,8}["']?)(?:(Bearer|Basic|Token|Digest)[ \t]+)?[^\s"',;]{4,}/gi,
    (_, prefix, scheme) => `${prefix}${scheme ? `${scheme} ` : ''}${marker('secret')}`],
  [/\b((?:set-)?cookie["']?[ \t]{0,8}[:=][ \t]{0,8}["']?)[^\r\n"']{8,}/gi, (_, prefix) => `${prefix}${marker('secret')}`],
  [/\b(Bearer[ \t]+)[A-Za-z0-9._~+/-]{16,}=*/g, (_, prefix) => `${prefix}${marker('secret')}`],
  // Command lines: curl -u user:pass, --password value, sshpass -p, mysql -pvalue.
  [/((?:^|\s)(?:-u|--user|--proxy-user)(?:[ \t]+|=)["']?)[^\s:"']{0,64}:[^\s"']+/g, (_, prefix) => `${prefix}${marker('credentials')}`],
  [new RegExp(`((?:^|\\s)--?[a-z0-9-]{0,31}?${FLAG_NAME})([ \\t]+|=)(?!-)(["']?)[^\\s"']{4,}`, 'gi'),
    (_, flag, separator, quote) => `${flag}${separator}${quote}${marker('secret')}`],
  [/\b(sshpass[ \t]+-p[ \t]*["']?)[^\s"']+/g, (_, prefix) => `${prefix}${marker('secret')}`],
  [/\b((?:mysql|mysqldump|mysqladmin|mariadb)\b[^\n]{0,200}?[ \t]-p)\S{3,}/g, (_, prefix) => `${prefix}${marker('secret')}`],
  // Keep the setting name: it tells the classifier what kind of work this is.
  // A quoted value may contain spaces and delimiters.
  [new RegExp(`\\b([A-Za-z0-9_.-]{0,40}${SECRET_NAME}[A-Za-z0-9_.-]{0,40})${SEPARATOR}(["'])(?:(?!\\3)[^\\r\\n]){4,512}\\3`, 'gi'),
    (_, name, separator, quote) => `${name}${separator}${quote}${marker('secret')}${quote}`],
  [new RegExp(`\\b([A-Za-z0-9_.-]{0,40}${SECRET_NAME}[A-Za-z0-9_.-]{0,40})${SEPARATOR}(["']?)${VALUE}{4,}`, 'gi'),
    (_, name, separator, quote) => `${name}${separator}${quote}${marker('secret')}`],
  // Upper-case environment names ending in _KEY, _PASS or _AUTH (STRIPE_KEY,
  // DB_PASS), and the bare lowercase words pass and auth. Booleans stay readable.
  [new RegExp(`\\b([A-Z][A-Z0-9_]{0,40}[_-]KEY|(?:[A-Z][A-Z0-9_]{0,40}[_-])?(?:PASS|AUTH))${SEPARATOR}(["']?)${VALUE}{4,}`, 'g'),
    (_, name, separator, quote) => `${name}${separator}${quote}${marker('secret')}`],
  [new RegExp(`(?<![A-Za-z0-9_-])((?:pass|auth)["']?[ \\t]{0,8}[:=][ \\t]{0,8})(["']?)(?!(?:true|false|null|none|undefined)\\b)${VALUE}{4,}`, 'gi'),
    (_, prefix, quote) => `${prefix}${quote}${marker('secret')}`],
  [/\b[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]{1,253}\.[A-Za-z]{2,24}\b/g, marker('email')],
  [/\b[A-Z]{2}[0-9]{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,4})?\b/g, match => ibanValid(match) ? marker('iban') : match],
  // Major card networks only, so millisecond timestamps and IDs survive.
  [/\b(?:4|5[1-5]|2[2-7]|3[47]|6)(?:[0-9][ -]?){11,17}[0-9]\b/g, match => luhnValid(match) ? marker('card') : match],
  // Italian tax code (checksum-validated) and US social security numbers.
  [/\b[A-Z]{6}[0-9LMNPQRSTUV]{2}[A-EHLMPRST][0-9LMNPQRSTUV]{2}[A-Z][0-9LMNPQRSTUV]{3}[A-Z]\b/g, match => codiceFiscaleValid(match) ? marker('national_id') : match],
  [/\b(?!000|666|9[0-9]{2})[0-9]{3}-(?!00)[0-9]{2}-(?!0000)[0-9]{4}\b/g, marker('national_id')],
  // International phone numbers need a leading plus sign and 8 to 15 digits.
  [/(?<![\w+])\+[0-9][0-9 .()-]{6,20}[0-9]/g, match => phoneValid(match) ? marker('phone') : match],
]);

/** @param {string} text */
export function redactSensitive(text) {
  let result = text;
  for (const rule of RULES) {
    // @ts-ignore String.replace accepts either replacement form.
    result = typeof rule === 'function' ? rule(result) : result.replace(rule[0], rule[1]);
  }
  return result;
}
