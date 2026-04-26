from abc import ABC, abstractmethod
from o_value import OValue

class OBackend(ABC):
    """
    A language backend. One instance per persistent environment.
    For ephemeral expressions, a fresh instance is created and discarded.
    """
    
    @abstractmethod
    def execute(self, code: str, bindings: dict[str, OValue]) -> OValue:
        """
        Execute `code` in this environment with `bindings` available as variables.
        Return the result as an OValue.
        `bindings` maps variable names to their resolved OValues (from $var refs).
        """
        ...
    
    @abstractmethod
    def cleanup(self) -> None:
        """Called when a persistent env is garbage collected."""
        ...

