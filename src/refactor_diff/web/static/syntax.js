"use strict";

// Syntax highlighting by language, one line at a time. Each language turns a line into spans
// [start, end, className] and carries a small state between lines (e.g. inside a triple-quoted
// string), so a run of consecutive lines highlights like the whole file would.
// Add a language by registering it in LANGUAGES below.

const Syntax = (() => {
  const PY_KEYWORDS = new Set([
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
  ]);
  const PY_CONSTANTS = new Set(["True", "False", "None", "NotImplemented", "Ellipsis", "__debug__"]);
  const PY_SELF = new Set(["self", "cls"]);
  const PY_BUILTINS = new Set([
    "abs", "aiter", "all", "anext", "any", "ascii", "bin", "bool", "breakpoint", "bytearray",
    "bytes", "callable", "chr", "classmethod", "compile", "complex", "delattr", "dict", "dir",
    "divmod", "enumerate", "eval", "exec", "filter", "float", "format", "frozenset", "getattr",
    "globals", "hasattr", "hash", "help", "hex", "id", "input", "int", "isinstance",
    "issubclass", "iter", "len", "list", "locals", "map", "max", "memoryview", "min", "next",
    "object", "oct", "open", "ord", "pow", "print", "property", "range", "repr", "reversed",
    "round", "set", "setattr", "slice", "sorted", "staticmethod", "str", "sum", "super",
    "tuple", "type", "vars", "zip", "__import__",
  ]);
  const JS_KEYWORDS = new Set([
    "abstract", "as", "asserts", "async", "await", "break", "case", "catch", "class", "const",
    "continue", "debugger", "declare", "default", "delete", "do", "else", "enum", "export",
    "extends", "finally", "for", "from", "function", "get", "if", "implements", "import", "in",
    "infer", "instanceof", "interface", "is", "keyof", "let", "module", "namespace", "new", "of",
    "override", "private", "protected", "public", "readonly", "return", "satisfies", "set",
    "static", "switch", "throw", "try", "type", "typeof", "var", "void", "while", "with", "yield",
  ]);
  const JS_CONSTANTS = new Set(["true", "false", "null", "undefined", "NaN", "Infinity"]);
  const JS_SELF = new Set(["this", "super"]);
  const JS_BUILTINS = new Set([
    "any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown",
    "console", "globalThis", "window", "document", "process", "require", "module", "exports",
    "parseInt", "parseFloat", "isNaN", "isFinite", "setTimeout", "setInterval", "clearTimeout",
    "clearInterval", "fetch", "structuredClone",
  ]);
  const JS_DEFINES = { function: "sx-fn-def", class: "sx-type", interface: "sx-type",
    type: "sx-type", enum: "sx-type", namespace: "sx-type" };
  const JS_IDENT = /[\p{L}_$][\p{L}\p{N}_$]*/uy;
  const JS_NUMBER = /(?:0[xX][\da-fA-F_]+|0[oO][0-7_]+|0[bB][01_]+|(?:\d[\d_]*(?:\.[\d_]*)?|\.\d[\d_]*)(?:[eE][+-]?\d+)?n?)/y;
  const REGEX_BODY = /\/(?:\\.|\[(?:\\.|[^\]\\])*\]|[^/\\\n[])+\/[a-z]*/y;
  const IDENT = /[\p{L}_][\p{L}\p{N}_]*/uy;
  const NUMBER = /(?:0[xX][\da-fA-F_]+|0[oO][0-7_]+|0[bB][01_]+|(?:\d[\d_]*(?:\.[\d_]*)?|\.\d[\d_]*)(?:[eE][+-]?\d+)?[jJ]?)/y;
  const STRING_START = /([rRbBuUfF]{0,2})('''|"""|'|")/y;

  // Python. State: null, or the open triple quote and whether the string is raw.
  const python = {
    start: () => null,
    line(text, state) {
      const spans = [];
      let i = 0;
      let prevWord = null; // previous identifier/keyword, for "def name" / "class Name"

      if (state) {
        const end = findClose(text, 0, state.quote, state.raw);
        if (end < 0) return { spans: [[0, text.length, "sx-str"]], state };
        spans.push([0, end, "sx-str"]);
        i = end;
        state = null;
      }
      const lead = text.match(/^\s*/)[0].length;
      while (i < text.length) {
        const ch = text[i];
        if (ch === " " || ch === "\t") { i++; continue; }
        if (ch === "#") { spans.push([i, text.length, "sx-com"]); break; }
        if (ch === "@" && i === lead) {
          IDENT.lastIndex = i + 1;
          let end = i + 1;
          while (IDENT.exec(text)) {
            end = IDENT.lastIndex;
            if (text[end] !== ".") break;
            IDENT.lastIndex = end + 1;
          }
          spans.push([i, end, "sx-deco"]);
          i = end;
          continue;
        }
        STRING_START.lastIndex = i;
        const s = STRING_START.exec(text);
        if (s && (s[1] === "" || !/[\p{L}\p{N}_]/u.test(text[i - 1] ?? ""))) {
          const raw = /[rR]/.test(s[1]);
          const quote = s[2];
          const end = findClose(text, i + s[0].length, quote, raw);
          if (end < 0) {
            spans.push([i, text.length, "sx-str"]);
            if (quote.length === 3) state = { quote, raw };
            break;
          }
          spans.push([i, end, "sx-str"]);
          i = end;
          prevWord = null;
          continue;
        }
        IDENT.lastIndex = i;
        const id = IDENT.exec(text);
        if (id) {
          const word = id[0];
          const end = i + word.length;
          const cls = classify(word, prevWord, text, end);
          if (cls) spans.push([i, end, cls]);
          prevWord = word;
          i = end;
          continue;
        }
        NUMBER.lastIndex = i;
        const num = /\d|\./.test(ch) ? NUMBER.exec(text) : null;
        if (num && num[0] !== ".") {
          spans.push([i, i + num[0].length, "sx-num"]);
          i += num[0].length;
          continue;
        }
        prevWord = null;
        i++;
      }
      return { spans, state };
    },
  };

  // TypeScript and JavaScript. State: null, or { mode: "comment" } inside /* */, or
  // { mode: "template" } inside a template literal that spans lines.
  const typescript = {
    start: () => null,
    line(text, state) {
      const spans = [];
      let i = 0;
      let prevWord = null; // previous identifier/keyword, for "function name" / "class Name"
      let prevSig = ""; // previous significant character, to tell a regex from a division

      if (state) {
        const end = state.mode === "comment"
          ? closeComment(text, 0)
          : findClose(text, 0, "`", false);
        const cls = state.mode === "comment" ? "sx-com" : "sx-str";
        if (end < 0) return { spans: [[0, text.length, cls]], state };
        spans.push([0, end, cls]);
        i = end;
        state = null;
        prevSig = "x";
      }
      while (i < text.length) {
        const ch = text[i];
        if (ch === " " || ch === "\t") { i++; continue; }
        if (text.startsWith("//", i)) { spans.push([i, text.length, "sx-com"]); break; }
        if (text.startsWith("/*", i)) {
          const end = closeComment(text, i + 2);
          if (end < 0) {
            spans.push([i, text.length, "sx-com"]);
            state = { mode: "comment" };
            break;
          }
          spans.push([i, end, "sx-com"]);
          i = end;
          continue;
        }
        if (ch === "@") {
          JS_IDENT.lastIndex = i + 1;
          let end = i + 1;
          while (JS_IDENT.exec(text)) {
            end = JS_IDENT.lastIndex;
            if (text[end] !== ".") break;
            JS_IDENT.lastIndex = end + 1;
          }
          if (end > i + 1) {
            spans.push([i, end, "sx-deco"]);
            i = end;
            prevSig = "x";
            continue;
          }
        }
        if (ch === "'" || ch === '"' || ch === "`") {
          const end = findClose(text, i + 1, ch, false);
          if (end < 0) {
            spans.push([i, text.length, "sx-str"]);
            if (ch === "`") state = { mode: "template" };
            break;
          }
          spans.push([i, end, "sx-str"]);
          i = end;
          prevWord = null;
          prevSig = "x";
          continue;
        }
        if (ch === "/" && (/^$|[(,=:[!&|?{};+\-*%<>~^]/.test(prevSig) ||
            ["return", "typeof", "case", "in", "of", "yield", "await"].includes(prevWord))) {
          REGEX_BODY.lastIndex = i;
          const re = REGEX_BODY.exec(text);
          if (re) {
            spans.push([i, i + re[0].length, "sx-str"]);
            i += re[0].length;
            prevSig = "x";
            prevWord = null;
            continue;
          }
        }
        JS_IDENT.lastIndex = i;
        const id = JS_IDENT.exec(text);
        if (id) {
          const word = id[0];
          const end = i + word.length;
          const cls = classifyJs(word, prevWord, text, end);
          if (cls) spans.push([i, end, cls]);
          prevWord = word;
          prevSig = JS_KEYWORDS.has(word) ? "" : "x";
          i = end;
          continue;
        }
        JS_NUMBER.lastIndex = i;
        const num = /\d|\./.test(ch) ? JS_NUMBER.exec(text) : null;
        if (num && num[0] !== ".") {
          spans.push([i, i + num[0].length, "sx-num"]);
          i += num[0].length;
          prevSig = "x";
          continue;
        }
        prevWord = null;
        prevSig = ch === ")" || ch === "]" ? "x" : ch;
        i++;
      }
      return { spans, state };
    },
  };

  function classifyJs(word, prevWord, text, end) {
    const before = text[end - word.length - 1];
    if (before === "." || before === "#") {
      return /^\s*\(/.test(text.slice(end)) ? "sx-fn" : null; // member: obj.method()
    }
    if (JS_DEFINES[prevWord] && !JS_KEYWORDS.has(word)) return JS_DEFINES[prevWord];
    if (JS_CONSTANTS.has(word)) return "sx-const";
    if (JS_SELF.has(word)) return "sx-self";
    if (JS_KEYWORDS.has(word)) {
      // Contextual keywords used as plain names: "type" as a property, "get" as a call, ...
      return /^\s*[:(]/.test(text.slice(end)) && !/^(if|for|while|switch|catch|function|return)$/.test(word)
        ? null
        : "sx-kw";
    }
    if (JS_BUILTINS.has(word)) return "sx-builtin";
    if (/^_*[A-Z]/.test(word) && /[a-z]/.test(word)) return "sx-type"; // PascalCase: types
    if (/^\s*(?:<[^()]*>)?\s*\(/.test(text.slice(end))) return "sx-fn";
    return null;
  }

  // Index just past the closing */, or -1 when the comment continues past this line.
  function closeComment(text, from) {
    const end = text.indexOf("*/", from);
    return end < 0 ? -1 : end + 2;
  }

  function classify(word, prevWord, text, end) {
    if (prevWord === "def") return "sx-fn-def";
    if (prevWord === "class") return "sx-type";
    if (PY_CONSTANTS.has(word)) return "sx-const";
    if (PY_KEYWORDS.has(word)) return "sx-kw";
    if (PY_SELF.has(word)) return "sx-self";
    const called = /^\s*\(/.test(text.slice(end));
    if (PY_BUILTINS.has(word) && text[end - word.length - 1] !== ".") return "sx-builtin";
    if (/^_*[A-Z]/.test(word) && /[a-z]/.test(word)) return "sx-type"; // CapWords: classes
    if (called) return "sx-fn";
    return null;
  }

  // Index just past the closing quote, or -1 when the string continues past this line.
  function findClose(text, from, quote, raw) {
    for (let i = from; i < text.length; i++) {
      if (text[i] === "\\" && !raw) { i++; continue; }
      if (text.startsWith(quote, i)) return i + quote.length;
    }
    return -1;
  }

  const LANGUAGES = [
    { test: /\.pyi?$/, lang: python },
    { test: /\.[cm]?[jt]sx?$/, lang: typescript },
  ];

  return {
    forPath(path) {
      const hit = path && LANGUAGES.find((l) => l.test.test(path));
      return hit ? hit.lang : null;
    },
  };
})();
