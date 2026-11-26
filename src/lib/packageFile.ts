export async function readPackageFile(file: File) {
  if (file.size > 24 * 1024 * 1024) throw new Error('Bundle exceeds 24 MiB.');
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = '';
  for (let offset = 0; offset < bytes.length; offset += 32768)
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 32768));
  return btoa(binary);
}
