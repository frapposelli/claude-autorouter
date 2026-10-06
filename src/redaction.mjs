// @ts-check
// Pattern-based redaction for evaluator excerpts. Classification needs the
// shape of a task, not credentials or personal identifiers, so recognizable
// values are replaced before any excerpt leaves the inference path. This is a
// best-effort filter: unrecognized formats can remain, and code that merely
// resembles an assignment can be over-redacted. Every quantifier is bounded or
// excludes its delimiter, so scanning stays linear in the input length.

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

/** @type {ReadonlyArray<readonly [RegExp, string | ((...match: string[]) => string)] | ((text: string) => string)>} */
const RULES = Object.freeze([
  // A block cut off by excerpting is still redacted through the end of text.
  [/-----BEGIN [A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----[\s\S]*?(?:-----END [A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----|$)/g, marker('private_key')],
  redactKeyTails,
  [/\b([a-z][a-z0-9+.-]{1,20}:\/\/)[^\s:@/]{1,256}:[^\s@/]{1,256}@/gi, (_, scheme) => `${scheme}${marker('credentials')}@`],
  [/\bsk-[A-Za-z0-9_-]{20,}/g, marker('secret')],
  [/\b[rs]k_(?:live|test)_[A-Za-z0-9]{16,}/g, marker('secret')],
  [/\b(?:AKIA|ASIA|ABIA|ACCA)[A-Z0-9]{16}\b/g, marker('secret')],
  [/\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})/g, marker('secret')],
  [/\bglpat-[A-Za-z0-9_-]{20,}/g, marker('secret')],
  [/\bxox[abposr]-[A-Za-z0-9-]{10,}/g, marker('secret')],
  [/https:\/\/hooks\.slack\.com\/services\/[A-Za-z0-9/]{8,}/g, marker('secret')],
  [/\bAIza[0-9A-Za-z_-]{35}/g, marker('secret')],
  [/\bnpm_[A-Za-z0-9]{36}/g, marker('secret')],
  [/\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/g, marker('secret')],
  [/\b((?:proxy-)?authorization["']?[ \t]{0,8}[:=][ \t]{0,8}["']?)(?:(Bearer|Basic|Token|Digest)[ \t]+)?[^\s"',;]{4,}/gi,
    (_, prefix, scheme) => `${prefix}${scheme ? `${scheme} ` : ''}${marker('secret')}`],
  [/\b(Bearer[ \t]+)[A-Za-z0-9._~+/-]{16,}=*/g, (_, prefix) => `${prefix}${marker('secret')}`],
  // Keep the setting name: it tells the classifier what kind of work this is.
  [/\b([A-Za-z0-9_.-]{0,40}(?:passw(?:or)?d|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|credentials?)[A-Za-z0-9_.-]{0,40})(["']?[ \t]{0,8}[:=][ \t]{0,8})(["']?)[^\s"',;]{4,}/gi,
    (_, name, separator, quote) => `${name}${separator}${quote}${marker('secret')}`],
  [/\b[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]{1,253}\.[A-Za-z]{2,24}\b/g, marker('email')],
  [/\b[A-Z]{2}[0-9]{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,4})?\b/g, match => ibanValid(match) ? marker('iban') : match],
  // Major card networks only, so millisecond timestamps and IDs survive.
  [/\b(?:4|5[1-5]|2[2-7]|3[47]|6)(?:[0-9][ -]?){11,17}[0-9]\b/g, match => luhnValid(match) ? marker('card') : match],
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
