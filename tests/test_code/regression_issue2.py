# Regression fixture for issue #2: annotated assignments (PEP 526) at module
# level must not crash the analyzer.  The original bug caused a panic when
# the AST visitor encountered `a: int = 3` because it tried to treat the
# annotation target like a plain-assignment target.

# Module-level annotated assignment with a value
a: int = 3
b = 4

# Module-level annotated assignment without a value (declaration only)
c: str
d: float = 1.5

# Chained / complex types
items: list = []
mapping: dict = {}


def annotated_fn(x: int) -> str:
    result: str = str(x)
    return result


class Container:
    value: int = 0
    label: str

    def set_value(self, v: int) -> None:
        self.value = v
