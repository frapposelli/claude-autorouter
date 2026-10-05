// Active execution state is not an evaluator cache. Pending tools and the
// current human task survive cache expiry; only retired tasks use the idle TTL.
export class TurnState {
  constructor({ limit = 1000, idleTtlMs = 30 * 60 * 1000, now = Date.now } = {}) {
    this.limit = limit;
    this.idleTtlMs = idleTtlMs;
    this.now = now;
    this.records = new Set();
    this.aliases = new Map();
    this.attempts = new Map();
  }

  remove(record) {
    for (const key of record.keys) if (this.aliases.get(key) === record) this.aliases.delete(key);
    this.records.delete(record);
  }

  get(key) {
    const record = this.aliases.get(key);
    if (!record) return undefined;
    if (!record.active && !record.pending && record.expires <= this.now()) {
      this.remove(record);
      return undefined;
    }
    if (this.ambiguous(key)) return undefined;
    return record.pin;
  }

  ambiguous(key) {
    const record = this.aliases.get(key);
    return Boolean(record && record.keys[0] !== key && [...this.records].filter(other =>
      other.active && other.pin && other.scope === record.scope && other.keys.includes(key)).length > 1);
  }

  toolOwner(scope, ids) {
    if (!ids.length) return undefined;
    const matches = [...this.records].filter(record => record.scope === scope && record.pin?.confirmed
      && ids.every(id => record.pin.toolModels?.some(tool => tool.id === id)));
    return matches.length === 1 ? { key: matches[0].keys[0], pin: matches[0].pin }
      : matches.length > 1 ? { ambiguous: true } : undefined;
  }

  /**
   * @param {string[]} keys
   * @param {any} pin
   * @param {{scope?:string,requestId?:string,sequence?:number}} [options]
   */
  select(keys, pin, { scope = '', requestId, sequence = 0 } = {}) {
    // Rejected admission must not supersede an already accepted attempt.
    if (requestId && (this.attempts.size >= this.limit || this.attempts.has(requestId))) return false;
    // An explicit new prompt identity starts a new task even when its text is
    // identical to an older task. Content aliases are lookup fallbacks, never
    // authority to merge two different primary identities.
    let record = this.aliases.get(keys[0]);
    if (!record) {
      for (const old of this.records) {
        if (!old.active && !old.pending && (old.expires <= this.now() || this.records.size >= this.limit)) this.remove(old);
      }
      // Never evict an active task just because other agents fill the cache.
      // The caller reports the capacity limit instead of inventing continuity.
      if (this.records.size >= this.limit) return false;
      record = { keys: [], scope, active: true, pending: 0, sequence, createdSequence: sequence, pin: undefined };
      this.records.add(record);
    }
    record.sequence = Math.max(record.sequence, sequence);
    for (const key of keys) {
      if (record.keys.includes(key)) continue;
      record.keys.push(key);
      // Stage secondary aliases privately until execution succeeds. Otherwise
      // a failed same-content task could destroy an older confirmed lookup.
      if (record.keys.length === 1) this.aliases.set(key, record);
      // Keep the primary identity and bounded recent discovery aliases.
      if (record.keys.length > 8) {
        const [expired] = record.keys.splice(1, 1);
        if (this.aliases.get(expired) === record) this.aliases.delete(expired);
      }
    }
    if (requestId) {
      record.pending++;
      this.attempts.set(requestId, { record, pin, sequence });
    } else {
      // Embedders that only call route() have selection evidence, not a
      // provider confirmation. The HTTP gateway always uses request IDs.
      this.commit(record, { ...pin, confirmed: false }, sequence);
    }
    return true;
  }

  commit(record, pin, sequence) {
    if (sequence !== record.sequence) return;
    record.pin = pin;
    for (const key of record.keys) {
      const current = this.aliases.get(key);
      if (!current || !current.pin || current === record || record.createdSequence >= current.createdSequence) this.aliases.set(key, record);
    }
    // Task age, not the order of later tool requests, determines retirement.
    record.active = Boolean(pin.toolModels?.length) || ![...this.records].some(other =>
      other !== record && other.scope === record.scope && other.pin && other.createdSequence > record.createdSequence);
    if (!record.active) record.expires = this.now() + this.idleTtlMs;
    for (const old of this.records) {
      if (old !== record && old.scope === record.scope && old.createdSequence < record.createdSequence && !old.pending && !old.pin?.toolModels?.length) {
        old.active = false;
        old.expires = this.now() + this.idleTtlMs;
      }
    }
  }

  complete(requestId, evidence) {
    const attempt = this.attempts.get(requestId);
    if (!attempt) return false;
    this.attempts.delete(requestId);
    const { record, pin, sequence } = attempt;
    record.pending--;
    const model = evidence?.continuation_model;
    if (typeof model === 'string' && model && sequence === record.sequence) {
      const toolModels = evidence.tool_uses ?? [];
      // Ambiguous or unbounded tool ownership must not be committed as fact.
      if (Array.isArray(toolModels) && toolModels.length <= 1000
        && toolModels.every(tool => typeof tool?.id === 'string' && tool.id.length > 0 && tool.id.length <= 256 && tool.model === model)) {
        this.commit(record, { ...pin, model, confirmed: true, toolModels }, sequence);
        return true;
      }
    }
    if (!record.pin && !record.pending) this.remove(record);
    else if (!record.pending && !record.pin?.toolModels?.length && [...this.records].some(other =>
      other !== record && other.scope === record.scope && other.pin && other.createdSequence > record.createdSequence)) {
      record.active = false;
      record.expires = this.now() + this.idleTtlMs;
    }
    return false;
  }
}
