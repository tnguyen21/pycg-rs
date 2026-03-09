# Regression fixture for issue #5: files that import from external packages
# (not installed or not on the analysis path) and files that use relative
# imports whose targets don't exist as real modules must not crash the analyzer.

# External package imports – these packages are NOT installed; the analyzer
# must handle unresolved imports gracefully.
import os.path
import numpy as np  # noqa: F401  # external, may not be installed
import pandas.io.parsers  # noqa: F401  # external, may not be installed

# Relative imports whose targets don't exist as real files under test_code/ –
# mirrors the original issue5/relimport.py fixture exactly.
from . import mod1  # noqa: F401  # test fixture – mod1 does not exist
from . import mod1 as moo  # noqa: F401  # test fixture
from ..mod3 import bar  # noqa: F401  # test fixture – mod3 does not exist
from .mod2 import foo  # noqa: F401  # test fixture – mod2 does not exist


class MyProcessor:
    """Uses an external-dep attribute path (pandas.io.parsers) in its body."""

    def __init__(self, path: str):
        if not os.path.isfile(path):
            raise FileNotFoundError(path)
        self.data = pandas.io.parsers.read_csv(path)

    def process(self):
        return self.data
