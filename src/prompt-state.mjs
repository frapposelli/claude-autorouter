import { redactSensitive } from './redaction.mjs';

// Claude Code prepends these as separate text blocks. Ignore only complete
// wrapper blocks; a user's text that mentions a tag or mixes it with a task
// must remain part of the classifier's input.
function isReminderBlock(text) {
  let remaining = text.trim();
  let found = false;
  while (remaining) {
    const opening = /^<(system-reminder|available-deferred-tools)>/.exec(remaining);
    if (!opening) return false;
    const closing = `</${opening[1]}>`;
    const end = remaining.indexOf(closing, opening[0].length);
    if (end < 0) return false;
    found = true;
    remaining = remaining.slice(end + closing.length).trimStart();
  }
  return found;
}

// Read explicit text only. Tool inputs, image bytes, and signed reasoning are
// never classifier material. Tool results still provide useful task context.
function contentText(content, omitReminders = false) {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  const parts = [];
  for (const block of content) {
    if (!block || typeof block !== 'object') continue;
    if (block.type === 'text') {
      const text = String(block.text ?? '');
      if (!omitReminders || !isReminderBlock(text)) parts.push(text);
    } else if (block.type === 'tool_result') {
      parts.push(`[tool result${block.is_error ? ' ERROR' : ''}] ${contentText(block.content)}`);
    } else if (block.type === 'tool_use') {
      parts.push(`[tool call: ${String(block.name ?? '').slice(0, 100)}]`);
    } else if (['image', 'document', 'thinking', 'redacted_thinking'].includes(block.type)) {
      parts.push(`[${block.type} omitted]`);
    } else {
      parts.push('[non-text content omitted]');
    }
  }
  return parts.join('\n');
}

function excerpt(text, length) {
  if (text.length <= length) return text;
  const marker = '\n[... omitted ...]\n';
  const retained = Math.max(0, length - (length > marker.length + 2 ? marker.length : 0));
  const head = Math.ceil(retained / 2);
  const tail = retained - head;
  return text.slice(0, head) + (length > marker.length + 2 ? marker : '') + (tail ? text.slice(-tail) : '');
}

const textCost = text => JSON.stringify(text).length - 2;

// Redact before excerpting so a value cannot be split into an unrecognizable
// fragment. Very long text keeps only end windows wider than any retained
// excerpt; the margin also keeps a value cut at a window edge out of the result.
const REDACTION_MARGIN = 4096;
function classifierText(text, limit) {
  const span = limit + REDACTION_MARGIN;
  if (text.length > 2 * span) text = `${text.slice(0, span)}\n[... omitted ...]\n${text.slice(-span)}`;
  return redactSensitive(text);
}

// Budget serialized characters, including JSON escaping, rather than just
// raw text length. The two ends keep both an initial instruction and a final
// question visible when one long text block must be shortened.
function fitText(text, budget) {
  if (budget <= 0 || !text) return '';
  let low = 0;
  let high = Math.min(text.length, budget);
  let fitted = '';
  while (low <= high) {
    const length = Math.floor((low + high) / 2);
    const candidate = excerpt(text, length);
    if (textCost(candidate) <= budget) {
      fitted = candidate;
      low = length + 1;
    } else high = length - 1;
  }
  return fitted;
}

function humanTask(message) {
  if (message?.role !== 'user') return '';
  if (Array.isArray(message.content) && message.content.some(block => block?.type === 'tool_result')) return '';
  return contentText(message.content, true);
}

// Claude Code emits /goal Stop-hook feedback as user text. Recognize only its
// observed wrapper and a condition established by an earlier expanded command;
// ordinary messages mentioning hooks remain human tasks. This does not alter
// message content, and the feedback remains available as classifier history.
export function goalFeedbackIndexes(messages) {
  const indexes = new Set();
  if (!Array.isArray(messages)) return indexes;
  let condition;
  let shortCondition;
  let sawFullFeedback = false;
  for (let index = 0; index < messages.length; index++) {
    const message = messages[index];
    if (message?.role !== 'user') continue;
    const command = /^\s*<command-name>\/goal<\/command-name>\s*<command-message>goal<\/command-message>\s*<command-args>([\s\S]*?)<\/command-args>(?:\s|$)/.exec(humanTask(message));
    if (command) {
      const value = command[1].trim();
      // /goal with no arguments is a status query, not a replacement goal.
      if (!value) continue;
      condition = value.length <= 4000 && !/^(?:clear|stop|off|reset|none|cancel)$/i.test(value) ? value : undefined;
      shortCondition = undefined;
      sawFullFeedback = false;
      if (condition?.length > 500) {
        let prefix = condition.slice(0, 500);
        const last = prefix.charCodeAt(prefix.length - 1);
        if (last >= 0xd800 && last <= 0xdbff) prefix = prefix.slice(0, -1);
        shortCondition = `${prefix}… [+${condition.length - prefix.length} chars]`;
      }
      continue;
    }
    if (!condition || messages[index - 1]?.role !== 'assistant') continue;
    const text = typeof message.content === 'string' ? message.content
      : Array.isArray(message.content) && message.content.length === 1 && message.content[0]?.type === 'text'
        && typeof message.content[0].text === 'string' ? message.content[0].text : undefined;
    if (text === undefined) continue;
    const matches = label => {
      const prefix = `Stop hook feedback:\n[${label}]: `;
      return text.startsWith(prefix) && Boolean(text.slice(prefix.length).trim());
    };
    if (matches(condition)) {
      indexes.add(index);
      sawFullFeedback = true;
    } else if (sawFullFeedback && shortCondition && matches(shortCondition)) indexes.add(index);
  }
  return indexes;
}

// Optional diagnostic logs need only the current human text, not evaluator
// history or non-text placeholders. Bound collection before joining strings,
// and never visit tool input/output, attachments, or reasoning payloads.
export function promptExcerpt(body, maxChars = 500) {
  if (!Number.isSafeInteger(maxChars) || maxChars < 0) throw new TypeError('maxChars must be a nonnegative safe integer');
  const messages = body?.messages;
  if (!maxChars || !Array.isArray(messages)) return '';
  let feedbackIndexes;
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index];
    if (message?.role !== 'user') continue;
    const content = message.content;
    if (Array.isArray(content) && content.some(block => block?.type === 'tool_result')) continue;
    const standalone = typeof content === 'string' ? content
      : Array.isArray(content) && content.length === 1 && content[0]?.type === 'text'
        && typeof content[0].text === 'string' ? content[0].text : undefined;
    if (standalone?.startsWith('Stop hook feedback:\n[')) {
      feedbackIndexes ??= goalFeedbackIndexes(messages);
      if (feedbackIndexes.has(index)) continue;
    }
    const blocks = typeof content === 'string' ? [{ type: 'text', text: content }]
      : Array.isArray(content) ? content : [];
    const characters = [];
    // Collect past the retained length: redaction must see a value that
    // straddles the final boundary, and may shorten the text before the cut.
    const window = maxChars + REDACTION_MARGIN;
    let nonText = false;
    for (const block of blocks) {
      if (block?.type !== 'text' || typeof block.text !== 'string') {
        nonText = true;
        continue;
      }
      const value = block.text;
      // Also omit complete wrappers in string messages. Unlike classifier
      // input, a diagnostic excerpt should never log a reminder-only turn.
      if (!/\S/.test(value) || isReminderBlock(value)) continue;
      if (characters.length) characters.push('\n');
      for (const character of value) {
        if (characters.length >= window) break;
        characters.push(character);
      }
      if (characters.length >= window) break;
    }
    if (characters.length) return [...redactSensitive(characters.join('').toWellFormed())].slice(0, maxChars).join('').toWellFormed();
    // An image/document-only human turn is a new task with no safe excerpt;
    // do not incorrectly label it with the preceding human task's text.
    if (nonText) return '';
  }
  return '';
}

export function buildState(body, limit = 12000) {
  const messages = body.messages ?? [];
  const feedbackIndexes = goalFeedbackIndexes(messages);
  let firstTask = '';
  let currentTask = '';
  let currentIndex = -1;
  for (let index = 0; index < messages.length; index++) {
    if (feedbackIndexes.has(index)) continue;
    const task = humanTask(messages[index]);
    if (!task.trim()) continue;
    if (!firstTask) firstTask = task;
    currentTask = task;
    currentIndex = index;
  }
  const state = {
    system: '',
    original_task: '',
    current_task: '',
    recent_messages: [],
    message_count: messages.length,
    tool_count: body.tools?.length ?? 0,
    context_is_excerpt: true,
  };
  const remaining = () => Math.max(0, limit - JSON.stringify(state).length);
  // Reserve more than half of the budget for the actual latest human task
  // before considering reminders, original instructions, or tool results.
  state.current_task = fitText(classifierText(currentTask, limit), Math.min(remaining(), Math.floor(limit * 0.55)));
  state.original_task = fitText(classifierText(firstTask, limit), Math.min(2000, Math.floor(remaining() * 0.3)));
  state.system = fitText(classifierText(contentText(body.system, true), limit), Math.min(1000, Math.floor(remaining() * 0.3)));

  for (let index = messages.length - 1; index >= 0 && state.recent_messages.length < 8; index--) {
    // current_task already contains this message; leave room for actual
    // preceding conversation, especially the latest tool result or failure.
    if (index === currentIndex) continue;
    const text = contentText(messages[index].content, true);
    if (!text) continue;
    const entry = { role: messages[index].role, content: '' };
    const overhead = JSON.stringify(entry).length + (state.recent_messages.length ? 1 : 0);
    const budget = Math.min(3000, remaining() - overhead);
    if (budget < 1) break;
    entry.content = fitText(classifierText(text, limit), budget);
    state.recent_messages.unshift(entry);
  }
  return state;
}
