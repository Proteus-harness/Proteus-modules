"""Execute selected unchanged pinned Hermes AST nodes, without importing AIAgent."""
import ast
import hashlib
import json
from pathlib import Path
import subprocess
import typing

HERE = Path(__file__).resolve().parent
SOURCES = json.loads((HERE / "SOURCES.json").read_text())


class NativeSource:
    def __init__(self, root):
        self.root = Path(root).resolve()
        revision = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=self.root, text=True
        ).strip()
        if revision != SOURCES["revision"]:
            raise ValueError("Hermes checkout is not the pinned release commit")
        self._trees = {}
        for relative, expected in SOURCES["files"].items():
            path = self.root / relative
            content = path.read_bytes()
            if hashlib.sha256(content).hexdigest() != expected:
                raise ValueError("Pinned Hermes source changed: " + relative)
            if path.suffix == ".py":
                self._trees[relative] = ast.parse(content, filename=str(path))

    def node(self, relative, dotted_name):
        current = self._trees[relative]
        for name in dotted_name.split("."):
            current = next(
                node for node in current.body
                if isinstance(node, (ast.FunctionDef, ast.ClassDef)) and node.name == name
            )
        return current

    def execute(self, relative, names, namespace=None):
        namespace = {**vars(typing), "__name__": __name__, **(namespace or {})}
        nodes = [self.node(relative, name) for name in names]
        self.execute_nodes(relative, nodes, namespace)
        return namespace

    def execute_nodes(self, relative, nodes, namespace):
        # Only the module-level future directive is added; native node bodies,
        # decorators, control flow and source line numbers remain unchanged.
        future = ast.ImportFrom(module="__future__", names=[ast.alias(name="annotations")], level=0)
        tree = ast.fix_missing_locations(ast.Module(body=[future, *nodes], type_ignores=[]))
        exec(compile(tree, str(self.root / relative), "exec"), namespace)

    def todo(self):
        relative = "tools/todo_tool.py"
        tree = self._trees[relative]
        boundary = self.node(relative, "TodoStore").lineno
        constants = [node for node in tree.body
                     if isinstance(node, ast.Assign) and node.lineno < boundary]
        namespace = {**vars(typing), "__name__": __name__, "json": json}
        self.execute_nodes(relative, [*constants, self.node(relative, "TodoStore"),
                                      self.node(relative, "todo_tool")], namespace)
        return namespace

    def inline_todo(self, todo):
        relative = "agent/inline_tool_executors.py"
        namespace = self.execute(relative, ["InlineToolContext", "_call_tool", "_tool"], {
            "dataclass": __import__("dataclasses").dataclass,
            "import_module": lambda name: _only_todo(name, todo),
        })
        assignment = next(node for node in self._trees[relative].body
                          if isinstance(node, ast.AnnAssign)
                          and isinstance(node.target, ast.Name)
                          and node.target.id == "_RAW_INLINE_TOOL_EXECUTORS")
        value = next(value for key, value in zip(assignment.value.keys, assignment.value.values)
                     if isinstance(key, ast.Constant) and key.value == "todo_list")
        # Evaluate the native todo entry unchanged; other native tools are never loaded.
        executor = eval(compile(ast.Expression(value), str(self.root / relative), "eval"), namespace)
        return executor, namespace["InlineToolContext"]


def _only_todo(name, namespace):
    if name != "tools.todo_tool":
        raise AssertionError("Probe attempted an unselected native import: " + name)
    return __import__("types").SimpleNamespace(todo_tool=namespace["todo_tool"])
