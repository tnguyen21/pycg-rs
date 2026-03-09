"""Accuracy fixture: chained call result attribute resolution.

Covers:
  - make().m() — direct call result attribute access without intermediate binding.
  - Nested chain: outer().inner().leaf() where each step returns a typed object.
"""


class Widget:
    def render(self):
        pass


class Container:
    def contents(self):
        return Widget()


def make():
    """Returns a Widget instance."""
    return Widget()


def make_container():
    """Returns a Container instance."""
    return Container()


def direct_chain_caller():
    """make().render() — no intermediate variable."""
    make().render()


def two_hop_chain_caller():
    """make_container().contents().render() — two-hop chain."""
    make_container().contents().render()
