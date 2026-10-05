export const DECISION_RESPONSE_LIMIT = 64 * 1024;
export const MODEL_METADATA_LIMIT = 1024 * 1024;

// Cancellation is best effort and must never await an uncooperative body's
// cancel promise. Fetch's signal remains responsible for its network request.
export function cancelResponseBody(response, reason) {
  try { Promise.resolve(response.body?.cancel(reason)).catch(() => {}); } catch {}
}

/**
 * Read at most limit bytes, observing cancellation throughout body delivery.
 * @param {Response} response
 * @param {{signal?:AbortSignal,limit?:number}} [options]
 */
export async function readBoundedJson(response, { signal, limit = DECISION_RESPONSE_LIMIT } = {}) {
  if (!Number.isSafeInteger(limit) || limit < 1) throw new Error('classifier_invalid_response');
  let reader;
  let bytes;
  let total = 0;
  const cancel = () => {
    try { Promise.resolve(reader?.cancel(signal?.reason)).catch(() => {}); } catch {}
  };
  try {
    signal?.throwIfAborted();
    reader = response.body?.getReader();
    if (!reader) throw new Error('classifier_invalid_response');
    signal?.addEventListener('abort', cancel, { once: true });
    // Header sizes are only an early rejection. The streamed byte count is
    // authoritative for chunked, compressed, absent or inaccurate lengths.
    const length = response.headers?.get('content-length');
    if (length && /^\d+$/.test(length) && Number(length) > limit) throw new Error('classifier_invalid_response');
    while (true) {
      signal?.throwIfAborted();
      const { value, done } = await reader.read();
      signal?.throwIfAborted();
      if (done) break;
      if (!(value instanceof Uint8Array) || value.byteLength > limit - total) throw new Error('classifier_invalid_response');
      const next = total + value.byteLength;
      // A single growable buffer also bounds metadata: a malicious one-byte
      // chunk stream must not retain tens of thousands of Buffer objects.
      if (!bytes || next > bytes.length) {
        const capacity = Math.min(limit, Math.max(next, bytes ? bytes.length * 2 : Math.min(1024, limit)));
        const grown = Buffer.allocUnsafe(capacity);
        bytes?.copy(grown, 0, 0, total);
        bytes = grown;
      }
      bytes.set(value, total);
      total = next;
    }
    try { return JSON.parse(bytes?.toString('utf8', 0, total) ?? ''); }
    catch { throw new Error('classifier_invalid_response'); }
  } finally {
    signal?.removeEventListener('abort', cancel);
    if (reader) { cancel(); try { reader.releaseLock(); } catch {} }
    else cancelResponseBody(response, signal?.reason);
  }
}
