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

export function buildState(body, limit = 12000) {
  const messages = body.messages ?? [];
  let firstTask = '';
  let currentTask = '';
  let currentIndex = -1;
  for (let index = 0; index < messages.length; index++) {
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
  state.current_task = fitText(currentTask, Math.min(remaining(), Math.floor(limit * 0.55)));
  state.original_task = fitText(firstTask, Math.min(2000, Math.floor(remaining() * 0.3)));
  state.system = fitText(contentText(body.system, true), Math.min(1000, Math.floor(remaining() * 0.3)));

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
    entry.content = fitText(text, budget);
    state.recent_messages.unshift(entry);
  }
  return state;
}
