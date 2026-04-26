from o_parser import TypedExpr, RawText, VarRef, ONode
from o_value import OValue, OStr, ONull, OBlob
from o_backend import OBackend
from backends.python_backend import PythonBackend
from backends.html_backend import HTMLBackend    # you'll write this
from typing import Optional

class ORuntime:
    def __init__(self):
        # Persistent environments: (lang, env_id) -> OBackend instance
        self._envs: dict[tuple[str, int], OBackend] = {}
        
        # Variable bindings: name -> OValue (lexically scoped in the expression tree)
        self._vars: dict[str, OValue] = {}
        
        # Backend factory: which class handles which lang tag
        self._backend_classes = {
            'python': PythonBackend,
            'html':   HTMLBackend,
            # 'haskell': HaskellBackend, etc.
        }
    
    def eval(self, node: ONode, local_vars: dict = None) -> OValue:
        vars = {**self._vars, **(local_vars or {})}
        
        match node:
            case RawText(text):
                return OStr(text)
            
            case VarRef(name):
                if name not in vars:
                    raise NameError(f"Unbound variable: ${name}")
                return vars[name]
            
            case TypedExpr(lang='O', env_id=_, body=body):
                # O's own evaluator: evaluate body in sequence, return last value
                result = ONull()
                for child in body:
                    result = self.eval(child, vars)
                return result
            
            case TypedExpr(lang=lang, env_id=env_id, body=body):
                # 1. Resolve body: collect raw text and splice $var values
                code_parts = []
                bindings = {}
                
                for child in body:
                    match child:
                        case RawText(text):
                            code_parts.append(text)
                        case VarRef(name):
                            val = vars.get(name, ONull())
                            bindings[name] = val
                            code_parts.append(self._splice(val))
                        case TypedExpr() as nested:
                            # Evaluate nested expression, splice its value
                            val = self.eval(nested, vars)
                            code_parts.append(self._splice(val))
                
                code = ''.join(code_parts)
                
                # 2. Get or create the appropriate backend
                backend = self._get_backend(lang, env_id)
                
                # 3. Execute
                result = backend.execute(code, bindings)
                
                # 4. Cleanup if ephemeral
                if env_id is None:
                    backend.cleanup()
                
                return result
    
    def _get_backend(self, lang: str, env_id: Optional[int]) -> OBackend:
        if lang not in self._backend_classes:
            raise ValueError(f"Unknown language: '{lang}'")
        
        if env_id is None:
            # Ephemeral: fresh backend, will be discarded after execution
            return self._backend_classes[lang]()
        
        key = (lang, env_id)
        if key not in self._envs:
            self._envs[key] = self._backend_classes[lang]()
        return self._envs[key]
    
    def _splice(self, val: OValue) -> str:
        """Convert an OValue to its string splice representation."""
        from o_value import OStr, OInt, OFloat, OBool, ONull, OBlob
        match val:
            case OStr(s):    return s
            case OInt(n):    return str(n)
            case OFloat(f):  return str(f)
            case OBool(b):   return str(b).lower()
            case ONull():    return ''
            case OBlob(data, mime):
                import base64
                b64 = base64.b64encode(data).decode()
                return f'data:{mime};base64,{b64}'
            case _:          return str(val)

