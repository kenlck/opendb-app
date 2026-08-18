# The shareable artifact is the Connection List

People share Connections as one exported bundle: Connection Strings with secrets, plus Names. The live list sits in app data, not Documents or git. Import merges by exact Connection String; duplicates skip; a different secret for the same Database is a second Connection. Treat the file as a credential dump.
