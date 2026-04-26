
;; This is the subprocess manager. It owns one subprocess per (lang, env-id) pair. It sends JSON commands, reads JSON responses, handles errors.

racket
#lang racket/base
(require racket/subprocess racket/match json "o-wire.rkt")

;; Backend registry: (lang . env-id) → process-handle
(define *registry* (make-hash))

(struct proc-handle (proc stdin stdout) #:transparent)

(define (get-or-spawn lang env-id)
  (define key (cons lang env-id))
  (hash-ref! *registry* key
    (λ ()
      (define shim-path (find-backend-shim lang))
      (define-values (proc out in _err)
        (subprocess #f #f #f shim-path))
      (proc-handle proc in out))))

(define (backend-exec lang env-id code bindings)
  (define handle (get-or-spawn lang env-id))
  (define msg (hasheq 'cmd "exec"
                       'code code
                       'bindings (hash-map/copy bindings
                                   (λ (k v) (values k (oval->jsexpr v))))))
  ;; Send
  (write-json msg (proc-handle-stdin handle))
  (newline (proc-handle-stdin handle))
  (flush-output (proc-handle-stdin handle))
  ;; Receive
  (define response (read-json (proc-handle-stdout handle)))
  (match (hash-ref response 'status)
    ["ok"  (jsexpr->oval (hash-ref response 'value))]
    ["err" (error 'o-backend "~a: ~a" lang (hash-ref response 'message))]))

(define (backend-cleanup lang env-id)
  (define key (cons lang env-id))
  (when (hash-has-key? *registry* key)
    (define handle (hash-ref *registry* key))
    (write-json (hasheq 'cmd "cleanup") (proc-handle-stdin handle))
    (newline (proc-handle-stdin handle))
    (flush-output (proc-handle-stdin handle))
    (read-json (proc-handle-stdout handle))  ; consume ack
    (subprocess-kill (proc-handle-proc handle) #t)
    (hash-remove! *registry* key)))

(provide backend-exec backend-cleanup)
