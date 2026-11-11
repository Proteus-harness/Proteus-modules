import { Effect } from "effect";
import { AIError, ProviderInternalError, TransportError } from "@opencode/ai";
import { policy } from "@opencode/core/session/runner/retry";

// One native retry schedule belongs to one sampling request. Confirmed tool
// results already belong to the request and survive each new model attempt.
export async function createRetryDelay(runtime, signal) {
    const decide = await Effect.runPromise(policy(runtime.conversation?.session_id ?? runtime.execution_id), {
        signal,
    });
    return async (failure) => {
        signal.throwIfAborted();
        const kind = failure.kind;
        let reason;
        if (kind === "stream_disconnected") {
            reason = new TransportError({ transport: "http", operation: "read", message: failure.message });
        } else if (kind && typeof kind === "object" && "retryable" in kind) {
            reason = new ProviderInternalError({
                message: failure.message,
                retryAfterMs: kind.retryable.retry_delay_ms ?? undefined,
            });
        } else return undefined;
        const decision = await Effect.runPromise(
            decide({
                cause: new AIError({ reason }),
                error: { type: "provider.transport", message: failure.message },
                agent: "build",
                model: { providerID: runtime.model_ref.provider, id: runtime.model_ref.model },
                hook: () => Effect.void,
                retry: true,
            }),
            { signal },
        );
        return decision.retry ? decision.delay : undefined;
    };
}
