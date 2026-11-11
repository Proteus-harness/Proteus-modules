/** One native SessionStep invocation owns this scheduling state. */
export function toolBarrier(specs, signal) {
    let fence = Promise.resolve();
    const parallel = new Set();
    const allowed = new Set(specs.filter(spec => spec.supports_parallel_tool_calls === true).map(spec => spec.name));
    return (call, effect) => {
        const start = () => { signal.throwIfAborted(); return effect(); };
        if (allowed.has(call.name)) {
            const task = fence.then(start);
            parallel.add(task);
            task.then(() => parallel.delete(task), () => parallel.delete(task));
            return task;
        }
        const task = Promise.all([fence, ...parallel].map(promise => promise.catch(() => {}))).then(start);
        fence = task.catch(() => {});
        return task;
    };
}
