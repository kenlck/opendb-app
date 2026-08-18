# Connection files are shareable and include plaintext secrets

The handful of people share Connections by passing the file. The secret travels in plaintext so the recipient can open a Session with the Client and nothing else. The OS vault is rejected. Anyone with the file has the Database password; treat Connection files like credentials.
