
; This is purely data. OValue struct definitions and their JSON encoding/decoding. No evaluation, no parsing, no subprocesses. Just the type system and its wire representation.

racket
#lang racket/base
(require racket/match json)

;; OValue is one of:
(struct o-null   ()              #:transparent)
(struct o-bool   (v)             #:transparent)
(struct o-int    (v)             #:transparent)  
(struct o-float  (v)             #:transparent)
(struct o-str    (v)             #:transparent)
(struct o-list   (items)         #:transparent)
(struct o-map    (entries)       #:transparent)  ; entries: hash string->OValue
(struct o-blob   (data mime)     #:transparent)  ; data: bytes

(define (oval->jsexpr v)
  (match v
    [(o-null)        (hasheq 't "null")]
    [(o-bool b)      (hasheq 't "bool"  'v b)]
    [(o-int n)       (hasheq 't "int"   'v n)]
    [(o-float f)     (hasheq 't "float" 'v f)]
    [(o-str s)       (hasheq 't "str"   'v s)]
    [(o-list items)  (hasheq 't "list"  'v (map oval->jsexpr items))]
    [(o-map entries) (hasheq 't "map"   'v (hash-map/copy entries
                                             (λ (k v) (values k (oval->jsexpr v)))))]
    [(o-blob d mime) (hasheq 't "blob"  'v (base64-encode d) 'mime mime)]))

(define (jsexpr->oval j)
  (match (hash-ref j 't)
    ["null"  (o-null)]
    ["bool"  (o-bool  (hash-ref j 'v))]
    ["int"   (o-int   (hash-ref j 'v))]
    ["float" (o-float (hash-ref j 'v))]
    ["str"   (o-str   (hash-ref j 'v))]
    ["list"  (o-list  (map jsexpr->oval (hash-ref j 'v)))]
    ["map"   (o-map   (hash-map/copy (hash-ref j 'v)
                        (λ (k v) (values k (jsexpr->oval v)))))]
    ["blob"  (o-blob  (base64-decode (hash-ref j 'v)) (hash-ref j 'mime))]))

(provide (all-defined-out))
