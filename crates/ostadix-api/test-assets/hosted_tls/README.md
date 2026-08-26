# Hosted TLS test identity

These CA and certificate files plus the PKCS#8 base64 key body are public test
fixtures. Tests reconstruct the PEM key only inside a private temporary
directory. This identity is not trusted by Ostadix and must never be used for a
deployed node.
