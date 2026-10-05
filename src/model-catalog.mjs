// Exact Claude API IDs only. A name containing "sonnet" or "opus" is not
// evidence that a gateway alias implements that model's request contract.
export const MODEL_CATALOG_REVIEWED_AT = '2026-10-05';
export const MODEL_CAPABILITY_SOURCES = Object.freeze({
  thinking: 'https://platform.claude.com/docs/en/build-with-claude/thinking-troubleshooting',
  effort: 'https://platform.claude.com/docs/en/build-with-claude/effort',
  sonnet55: 'https://platform.claude.com/docs/en/models/sonnet-5-5/migration-guide',
  opus55: 'https://platform.claude.com/docs/en/models/opus-5-5/whats-new-opus-5-5',
  auto: 'https://code.claude.com/docs/en/permission-modes',
  subscriptionContext: 'https://code.claude.com/docs/en/model-config',
});

/**
 * @typedef {object} ModelCapabilities
 * @property {'haiku'|'sonnet'|'opus'} family
 * @property {number} maxOutputTokens Standard Messages API limit, not Batch beta.
 * @property {number|undefined} contextWindow Unambiguous window across supported auth modes.
 * @property {boolean} capacityUpgradeSource Existing small/opt-in-window models.
 * @property {boolean} toolReferences
 * @property {boolean} autoMode Claude client Auto eligibility, not arbitrary request compatibility.
 * @property {boolean} sharedAuto Verified modern Auto execution pair.
 * @property {readonly string[]} thinkingTypes
 * @property {readonly string[]} effortLevels
 * @property {boolean} forcedToolChoice
 * @property {boolean} assistantPrefill
 * @property {boolean} defaultSamplingOnly
 * @property {boolean} midConversationSystem
 * @property {boolean} perMessageEffort
 * @property {boolean} taskBudget
 * @property {'adaptive'|'between_tools'|undefined} [disabledThinkingAdaptation]
 * @property {string} reviewedAt
 * @property {string} source
 */

const BASIC_EFFORT = Object.freeze(['low', 'medium', 'high']);
const FOUR_EFFORT = Object.freeze([...BASIC_EFFORT, 'max']);
const FIVE_EFFORT = Object.freeze([...BASIC_EFFORT, 'xhigh', 'max']);
const EXTENDED = Object.freeze(['disabled', 'enabled']);
const BOTH = Object.freeze(['disabled', 'enabled', 'adaptive']);
const ADAPTIVE = Object.freeze(['disabled', 'adaptive']);
const registry = {};

function add(ids, facts) {
  const canonical = ids[0].replace(/^claude-/, '');
  const entry = Object.freeze({
    contextWindow: 200000, maxOutputTokens: 64000, capacityUpgradeSource: true,
    toolReferences: true, autoMode: false, sharedAuto: false,
    thinkingTypes: EXTENDED, effortLevels: Object.freeze([]),
    forcedToolChoice: true, assistantPrefill: true, defaultSamplingOnly: false,
    midConversationSystem: false, perMessageEffort: false, taskBudget: false,
    reviewedAt: MODEL_CATALOG_REVIEWED_AT,
    source: `https://platform.claude.com/docs/en/models/${canonical}/overview`,
    ...facts,
  });
  for (const id of ids) registry[id] = entry;
}

add(['claude-haiku-4-5', 'claude-haiku-4-5-20251001'], { family: 'haiku' });
add(['claude-sonnet-4-5', 'claude-sonnet-4-5-20250929'], { family: 'sonnet' });
add(['claude-opus-4-5', 'claude-opus-4-5-20251101'], { family: 'opus', effortLevels: BASIC_EFFORT });
// 4.6 has a 1M API window, but subscription usage can need a client opt-in.
// Retain the existing conservative capacity policy instead of promising 1M.
for (const family of ['sonnet', 'opus']) add([`claude-${family}-4-6`], {
  family, contextWindow: undefined, maxOutputTokens: 128000, autoMode: true,
  thinkingTypes: BOTH, effortLevels: FOUR_EFFORT, assistantPrefill: false,
});
for (const version of ['4-7', '4-8']) add([`claude-opus-${version}`], {
  family: 'opus', contextWindow: 1000000, maxOutputTokens: 128000,
  capacityUpgradeSource: false, autoMode: true, thinkingTypes: ADAPTIVE,
  effortLevels: FIVE_EFFORT, assistantPrefill: false, defaultSamplingOnly: true,
});
for (const family of ['sonnet', 'opus']) for (const version of ['5', '5-5']) {
  const latest = version === '5-5';
  add([`claude-${family}-${version}`], {
    family, contextWindow: 1000000, maxOutputTokens: 128000,
    capacityUpgradeSource: false, autoMode: true, sharedAuto: true,
    thinkingTypes: latest ? Object.freeze(family === 'sonnet' ? ['adaptive', 'between_tools'] : ['adaptive']) : ADAPTIVE,
    effortLevels: FIVE_EFFORT, forcedToolChoice: !latest,
    assistantPrefill: false, defaultSamplingOnly: true,
    midConversationSystem: family === 'opus' || latest,
    perMessageEffort: family === 'opus' || latest,
    taskBudget: family === 'opus' || latest,
    // Preserve the existing explicit adaptation for Opus 5 as well as 5.5.
    disabledThinkingAdaptation: family === 'opus' ? 'adaptive' : latest ? 'between_tools' : undefined,
  });
}

export const MODEL_CATALOG = Object.freeze(registry);
/** @returns {Readonly<ModelCapabilities>|undefined} */
export const modelCapabilities = model => typeof model === 'string' && Object.hasOwn(MODEL_CATALOG, model)
  ? MODEL_CATALOG[model] : undefined;
export const modelContextWindow = model => modelCapabilities(model)?.contextWindow;
export const hasNativeMillionContext = model => modelContextWindow(model) === 1000000;
export const canUpgradeContext = model => modelCapabilities(model)?.capacityUpgradeSource === true;
export const supportsToolReferences = model => modelCapabilities(model)?.toolReferences === true;
export const supportsAutoMode = model => modelCapabilities(model)?.autoMode === true;
