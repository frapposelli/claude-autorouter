// Source-only, bounded scalar observations. No body observer or scheduling gate.
// Observer faults invalidate the final trace without replacing original errors.
import assert from 'node:assert/strict';

const MAX_ROWS = 512;
const MAX_ENCODED_ROWS = 24 * 1024;
const MAX_PATCHES = 96;
const FIELDS = ['sequence', 'event', 'request', 'connection', 'last_socket_owner_request',
  'socket_flags_known', 'socket_flags', 'request_reused_socket', 'request_destroyed',
  'agent_free_member', 'agent_active_member', 'agent_free_count', 'agent_active_count',
  'agent_queued_count', 'response_complete', 'response_readable_ended',
  'response_readable_length', 'error_code', 'had_error', 'original_threw'];
const SOCKET_FLAGS = ['readableEnded', 'writable', 'writableEnded', 'writableFinished', 'destroyed', 'closed'];
const boolean = value => typeof value === 'boolean' ? value : null;
const code = value => typeof value === 'string' && /^[A-Z0-9_]{1,60}$/.test(value) ? value : 'other';

export function createPassiveNodeLifecycle({ agent, requestNumbering, minimumRequest = 0,
  maximumRequests = 16, maximumConnections = 8, expectedRequests = 3 }) {
  assert.ok(agent && typeof agent.addRequest === 'function' && typeof agent.reuseSocket === 'function');
  assert.ok(['zero_based_session_path_ordinal', 'one_based_idle_submission'].includes(requestNumbering));
  assert.ok([0, 1].includes(minimumRequest));
  assert.ok(Number.isSafeInteger(maximumRequests) && maximumRequests >= 1 && maximumRequests <= 16);
  assert.ok(Number.isSafeInteger(maximumConnections) && maximumConnections >= 1 && maximumConnections <= 8);
  assert.ok(Number.isSafeInteger(expectedRequests) && expectedRequests >= 1 && expectedRequests <= maximumRequests);
  const rows = [], patches = [], contexts = [], requestIds = new Set();
  const requests = new WeakMap(), sockets = new WeakMap(), responses = new WeakMap();
  let failure = null, stopped = false, encodedRows = 2, connectionCount = 0, responseCount = 0;
  const require = (value, reason) => { if (!value) throw Error(reason); };
  const observe = action => {
    if (stopped || failure !== null) return;
    try { return action(); } catch { failure = 'observer_invariant'; }
  };
  function patch(target, name, factory) {
    require(patches.length < MAX_PATCHES, 'patch bound');
    const descriptor = Object.getOwnPropertyDescriptor(target, name);
    require(descriptor === undefined || ('value' in descriptor && descriptor.configurable), 'method descriptor');
    const original = target[name];
    require(typeof original === 'function', 'method function');
    const installed = factory(original);
    Object.defineProperty(target, name, { configurable: true, enumerable: descriptor?.enumerable ?? false,
      writable: true, value: installed });
    // Restoration metadata must not extend a request/response/socket lifetime.
    // The installed wrapper itself keeps its original callable alive while its
    // target is alive; all callable/object references in this list are weak.
    const saved = descriptor === undefined ? undefined : { configurable: descriptor.configurable,
      enumerable: descriptor.enumerable, writable: descriptor.writable, value: new WeakRef(descriptor.value) };
    patches.push({ target: new WeakRef(target), name, descriptor: saved, installed: new WeakRef(installed) });
  }
  function membership(table, socket) {
    const keys = Object.keys(table);
    require(keys.length <= 16, 'agent key bound');
    let count = 0, member = false;
    for (const key of keys) {
      const values = table[key];
      require(Array.isArray(values) && values.length <= maximumRequests, 'agent list bound');
      count += values.length;
      require(count <= maximumRequests, 'agent total bound');
      if (socket !== null && values.includes(socket)) member = true;
    }
    return [member, count];
  }
  function record(event, request = null, socket = null, response = null, extra = {}) {
    socket ??= request?.socket ?? null;
    const physical = socket === null ? null : sockets.get(socket);
    require(socket === null || physical !== undefined, 'socket identity');
    let known = 0, flags = 0;
    if (socket !== null) for (let index = 0; index < SOCKET_FLAGS.length; index++) {
      const value = socket[SOCKET_FLAGS[index]];
      if (typeof value === 'boolean') { known |= 1 << index; if (value) flags |= 1 << index; }
    }
    const [free, freeCount] = membership(agent.freeSockets, socket);
    const [active, activeCount] = membership(agent.sockets, socket);
    const [, queued] = membership(agent.requests, null);
    const buffered = response?.readableLength ?? null;
    require(buffered === null || (Number.isSafeInteger(buffered) && buffered >= 0 && buffered <= 1024 * 1024), 'response length scalar bound');
    const row = [rows.length + 1, event, request?.id ?? null, physical?.id ?? null,
      physical?.owner ?? null, known, flags, boolean(request?.object.reusedSocket), boolean(request?.object.destroyed),
      socket === null ? null : free, socket === null ? null : active, freeCount, activeCount, queued,
      boolean(response?.complete), boolean(response?.readableEnded), buffered,
      extra.error === undefined ? null : code(extra.error?.code), extra.hadError ?? null, extra.threw ?? null];
    require(row.length === FIELDS.length, 'closed row shape');
    const size = Buffer.byteLength(JSON.stringify(row)) + (rows.length ? 1 : 0);
    if (rows.length >= MAX_ROWS || size > MAX_ENCODED_ROWS - encodedRows) { failure = 'trace_bound'; return; }
    rows.push(row); encodedRows += size;
  }
  // The original function is called exactly once with unchanged receiver and
  // arguments. A failed observer never replaces the original return or throw.
  function delegate(original, receiver, args, before, after) {
    observe(before);
    let threw = true;
    try { const result = Reflect.apply(original, receiver, args); threw = false; return result; }
    finally { observe(() => after(threw)); }
  }
  function ownSocket(socket, request) {
    require(socket && typeof socket.emit === 'function', 'socket object');
    let physical = sockets.get(socket);
    if (physical === undefined) {
      require(connectionCount < maximumConnections, 'connection bound');
      physical = { id: ++connectionCount, owner: null };
      sockets.set(socket, physical);
      patch(socket, 'emit', original => function(event, ...args) {
        const selected = ['end', 'finish', 'error', 'close'].includes(event);
        const extra = { ...(event === 'error' ? { error: args[0] } : {}),
          ...(event === 'close' ? { hadError: boolean(args[0]) } : {}) };
        return delegate(original, this, arguments,
          () => { require(this === socket, 'socket emit receiver'); if (selected) record(`socket_${event}_before_emit`, null, socket, null, extra); },
          threw => { if (selected) record(`socket_${event}_after_emit`, null, socket, null, { ...extra, threw }); });
      });
    }
    // Ownership is the last actual request socket event. A reuseSocket call
    // names its candidate request separately and does not pre-confirm assignment.
    if (request !== null) { physical.owner = request.id; request.socket = socket; }
    return physical;
  }
  function ownResponse(response, request) {
    require(response && typeof response.push === 'function' && typeof response.emit === 'function', 'response object');
    require(!responses.has(response) && responseCount < maximumRequests, 'response ownership');
    responseCount++; responses.set(response, request.id);
    patch(response, 'push', original => function(chunk) {
      let complete = false;
      return delegate(original, this, arguments,
        () => { require(this === response, 'response push receiver'); complete = chunk === null && this.complete === true; if (complete) record('producer_complete_before_push', request, null, response); },
        threw => { if (complete) record('producer_complete_after_push', request, null, response, { threw }); });
    });
    patch(response, 'emit', original => function(event, ...args) {
      const selected = ['end', 'aborted', 'error', 'close'].includes(event);
      const extra = event === 'error' ? { error: args[0] } : {};
      return delegate(original, this, arguments,
        () => { require(this === response, 'response emit receiver'); if (selected) record(`response_${event}_before_emit`, request, null, response, extra); },
        threw => { if (selected) record(`response_${event}_after_emit`, request, null, response, { ...extra, threw }); });
    });
  }
  function bindRequest(object) {
    const context = contexts.at(-1);
    require(context !== undefined, 'constructor invocation context');
    let request = requests.get(object);
    if (request !== undefined) { require(request.id === context.id, 'request context ownership'); return request; }
    require(context.request === null, 'single constructor request');
    request = { id: context.id, object, socket: null };
    requests.set(object, request); context.request = request;
    patch(object, 'emit', original => function(event, ...args) {
      const selected = ['socket', 'response', 'error', 'finish'].includes(event);
      const extra = event === 'error' ? { error: args[0] } : {};
      return delegate(original, this, arguments, () => {
        require(this === object, 'request emit receiver');
        if (event === 'socket') ownSocket(args[0], request);
        if (event === 'response') ownResponse(args[0], request);
        if (selected) record(`request_${event}_before_emit`, request, null, event === 'response' ? args[0] : null, extra);
      }, threw => {
        if (selected) record(`request_${event}_after_emit`, request, null, event === 'response' ? args[0] : null, { ...extra, threw });
      });
    });
    return request;
  }
  observe(() => {
    patch(agent, 'addRequest', original => function(request) {
      let owned = null;
      return delegate(original, this, arguments,
        () => { require(this === agent, 'agent add receiver'); owned = bindRequest(request); record('agent_add_request_before', owned); },
        threw => record('agent_add_request_after', owned, null, null, { threw }));
    });
    patch(agent, 'reuseSocket', original => function(socket, request) {
      let owned = null;
      return delegate(original, this, arguments,
        () => { require(this === agent, 'agent reuse receiver'); owned = requests.get(request); require(owned !== undefined, 'reuse request owner'); ownSocket(socket, null); owned.socket = socket; record('agent_reuse_socket_before', owned, socket); },
        threw => record('agent_reuse_socket_after', owned, socket, null, { threw }));
    });
    patch(agent, 'emit', original => function(event, socket) {
      return delegate(original, this, arguments, () => {}, threw => {
        require(this === agent, 'agent emit receiver');
        // This is after the real Agent free handler and remaining listeners.
        // Actual list membership, rather than a free event, proves admission.
        if (event === 'free') record('agent_free_after_emit', null, socket, null, { threw });
      });
    });
  });
  return {
    invoke(original, receiver, args, id) {
      const context = { id, request: null };
      observe(() => {
        require(Number.isSafeInteger(id) && id >= minimumRequest && id < minimumRequest + maximumRequests, 'request identity bound');
        require(requestIds.size < maximumRequests && !requestIds.has(id) && contexts.length === 0, 'unique unnested request invocation');
        requestIds.add(id); record('request_invocation', { id, object: {}, socket: null });
      });
      const pushed = contexts.length < maximumRequests;
      if (pushed) contexts.push(context);
      let threw = true, result;
      try { result = Reflect.apply(original, receiver, args); threw = false; return result; }
      finally {
        if (pushed) contexts.pop();
        observe(() => {
          require(pushed && (threw || context.request?.object === result), 'returned request identity');
          record('request_return', context.request, null, null, { threw });
        });
      }
    },
    finish() {
      if (!stopped) {
        observe(() => { require(contexts.length === 0 && requestIds.size === expectedRequests, 'complete fixed invocation scope'); record('observer_cleanup'); });
        // Restore every instance even when a previous observer failed.
        for (const item of patches.splice(0).reverse()) {
          try {
            const target = item.target.deref();
            if (target === undefined) continue; // A collected owner has nothing left to restore.
            const installed = item.installed.deref();
            require(installed !== undefined && target[item.name] === installed, 'wrapper replaced');
            if (item.descriptor === undefined) require(delete target[item.name], 'inherited method restoration');
            else {
              const value = item.descriptor.value.deref();
              require(value !== undefined, 'original method restoration');
              Object.defineProperty(target, item.name, { ...item.descriptor, value });
            }
          } catch { failure ??= 'restoration_invariant'; }
        }
        stopped = true;
      }
      return { schema_version: 1, available: true, complete: failure === null, failure,
        request_numbering: requestNumbering,
        connection_numbering: 'one_based_first_request_socket_or_real_reuse_observation',
        sequence_numbering: 'one_based_per_process_observer_boundary',
        socket_flag_bits: SOCKET_FLAGS, fields: FIELDS, rows,
        request_count: requestIds.size, connection_count: connectionCount, response_count: responseCount,
        limits: { rows: MAX_ROWS, encoded_rows_bytes: MAX_ENCODED_ROWS, patches: MAX_PATCHES, expected_requests: expectedRequests },
        observation: 'before/after JavaScript emit and HTTP producer push; no raw FIN or physical-close inference' };
    },
  };
}
