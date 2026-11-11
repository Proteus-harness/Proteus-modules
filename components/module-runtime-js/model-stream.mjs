import { setTimeout as sleep } from "node:timers/promises";

/**
 * Pi and OpenCode dispatch tools only after a terminal response. A failed
 * sampling attempt has made no tool effects; the request already includes
 * tool results from earlier accepted steps. Keep this retry inside sampling.
 * The owning workflow chooses the retry budget and delay.
 */
export async function receiveModelResponse(request, { hostCall, signal }, nextRetryDelay) {
    for (let retry = 1; ; retry++) {
        signal.throwIfAborted();
        await hostCall("host.events.emit", { event: { ModelRequestPrepared: { model: request.model } } });
        const cursor = await hostCall("host.model.stream.start", { request });
        for (;;) {
            signal.throwIfAborted();
            const item = await hostCall("host.model.stream.next", cursor);
            if (item.type === "message_completed") continue;
            if (item.type === "response") return item.response;
            if (item.type !== "error") throw new Error("Unknown model stream item " + item.type);

            const failure = item.failure;
            const kind = failure.kind;
            const retryable = kind === "stream_disconnected" || (kind != null && Object.hasOwn(kind, "retryable"));
            const delay = retryable ? await nextRetryDelay(failure, retry) : undefined;
            if (delay === undefined) {
                const error = new Error(failure.message);
                error.modelFailure = failure;
                throw error;
            }
            signal.throwIfAborted();
            await sleep(delay, undefined, { signal });
            break;
        }
    }
}
