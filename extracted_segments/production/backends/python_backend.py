import io, sys
from o_backend import OBackend
from o_value import OValue, py_to_oval, oval_to_py

class PythonBackend(OBackend):
    def __init__(self):
        self.globals = {}          # Persistent namespace for python[n] envs
    
    def execute(self, code: str, bindings: dict[str, OValue]) -> OValue:
        # Inject bound variables into the namespace
        for name, oval in bindings.items():
            self.globals[name] = oval_to_py(oval)
        
        # Capture stdout as a fallback "return value"
        old_stdout = sys.stdout
        sys.stdout = captured = io.StringIO()
        
        result = None
        try:
            # Try exec + check for __oval_result__ convention
            exec(compile(code, '<O-python>', 'exec'), self.globals)
            result = self.globals.pop('__oval_result__', None)
        finally:
            sys.stdout = old_stdout
        
        if result is not None:
            return py_to_oval(result)
        
        # Fall back to captured stdout as an OStr
        output = captured.getvalue()
        return py_to_oval(output) if output else py_to_oval(None)
    
    def cleanup(self):
        self.globals.clear()
The __oval_result__ convention is worth explaining: Python code that wants to explicitly pass a value to the next expression writes:

python
python[0]^(
    import matplotlib.pyplot as plt
    import io
    fig, ax = plt.subplots()
    ax.plot([1, 2, 3], [4, 5, 6])
    buf = io.BytesIO()
    fig.savefig(buf, format='png')
    __oval_result__ = ('blob', buf.getvalue(), 'image/png')
)_python[0]
And the backend converts the ('blob', bytes, mime) tuple to OBlob. Clean, explicit, no magic needed.

Part 4: The Evaluator
python
