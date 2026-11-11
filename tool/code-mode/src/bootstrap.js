(() => {
    'use strict';
    const invoke = globalThis.__codeModeCall;
    const print = globalThis.__codeModePrint;
    delete globalThis.__codeModeCall;
    delete globalThis.__codeModePrint;
    // Promise reactions are tracked; a fire-and-forget microtask API would
    // instead let job exceptions escape the script's root Promise.
    delete globalThis.queueMicrotask;
    const stringify = JSON.stringify.bind(JSON);
    const format = value => {
        if (typeof value === 'string') return value;
        const encoded = stringify(value);
        return encoded === undefined ? String(value) : encoded;
    };
    const call = function (name, args) {
        if (arguments.length !== 2) throw new TypeError('tools.call requires exactly two arguments: name, args');
        if (typeof name !== 'string' || !name.length) throw new TypeError('tools.call name must be a nonempty string');
        const encoded = stringify(args);
        if (encoded === undefined) throw new TypeError('tools.call arguments must be JSON serializable');
        return invoke(name, encoded);
    };
    Object.defineProperties(globalThis, {
        tools: { value: Object.freeze({call}), enumerable: true },
        text: { value: value => print(format(value)), enumerable: true },
        console: { value: Object.freeze({log: (...values) => print(values.map(format).join(' '))}), enumerable: true }
    });
})();
