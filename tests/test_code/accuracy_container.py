"""Golden accuracy fixture: container/subscript call flow.

Covers:
- list[i]() calls: functions stored in a list and called via integer subscript
- dict[key]() calls: functions stored in a dict and called via string key

These test the analyzer's ability to track call resolution through container literals.

Adapted from PyCG micro-benchmark lists/simple, dicts/simple.
"""


def func_a():
    pass


def func_b():
    pass


def func_c():
    pass


def list_subscript_caller():
    """Call via subscript on a function list — both func_a and func_b must be tracked."""
    funcs = [func_a, func_b]
    funcs[0]()
    funcs[1]()


def dict_subscript_caller():
    """Call via string key on a function dict — func_a and func_c must be tracked."""
    dispatch = {"a": func_a, "c": func_c}
    dispatch["a"]()
    dispatch["c"]()
