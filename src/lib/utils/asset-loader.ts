/**
 * src/lib/utils/asset-loader.ts
 * 
 * General-purpose utility to fetch and reassemble chunked assets.
 * Used to bypass 25MB file size limits on platforms like Cloudflare Pages.
 */

interface ChunkManifest {
    filename: string;
    chunks: number;
    totalSize: number;
    chunkSize: number;
}

/**
 * Fetches an asset, potentially reassembling it from chunks if a manifest exists.
 * @param url The base URL of the asset (e.g., /libreoffice-wasm/soffice.wasm)
 * @returns A Blob containing the reassembled or directly fetched asset
 */
export async function fetchAssembledBlob(url: string): Promise<Blob> {
    // Determine the manifest URL by stripping query parameters and appending .manifest.json
    const [baseUrl, query] = url.split('?');
    const queryString = query ? `?${query}` : '';
    const manifestUrl = `${baseUrl}.manifest.json${queryString}`;
    
    try {
        // Attempt to fetch the manifest
        const manifestRes = await fetch(manifestUrl);
        
        if (manifestRes.ok) {
            const manifest: ChunkManifest = await manifestRes.json();
            console.log(`[asset-loader] Manifest found for ${manifest.filename}. Reassembling from ${manifest.chunks} chunks...`);

            // Fetch all chunks in parallel
            const chunkPromises: Promise<ArrayBuffer>[] = [];
            for (let i = 0; i < manifest.chunks; i++) {
                const chunkUrl = `${baseUrl}.part_${i}${queryString}`;
                chunkPromises.push(
                    fetch(chunkUrl).then(res => {
                        if (!res.ok) throw new Error(`Failed to fetch chunk ${i} for ${url}`);
                        return res.arrayBuffer();
                    })
                );
            }

            const chunks = await Promise.all(chunkPromises);

            // Assemble chunks into a single buffer
            const assembled = new Uint8Array(manifest.totalSize);
            let offset = 0;
            for (const chunk of chunks) {
                assembled.set(new Uint8Array(chunk), offset);
                offset += chunk.byteLength;
            }

            // Determine MIME type based on extension
            let mimeType = 'application/octet-stream';
            const lowerUrl = url.toLowerCase();
            if (lowerUrl.includes('.wasm')) mimeType = 'application/wasm';
            else if (lowerUrl.includes('.js')) mimeType = 'application/javascript';
            else if (lowerUrl.includes('.ttf')) mimeType = 'font/ttf';
            else if (lowerUrl.includes('.otf')) mimeType = 'font/otf';
            else if (lowerUrl.includes('.woff2')) mimeType = 'font/woff2';

            return new Blob([assembled], { type: mimeType });
        }
    } catch (err) {
        // Fallback to direct fetch on any error
        console.debug(`[asset-loader] Manifest check failed or skipped for ${url}:`, err);
    }

    // Fallback: Fetch the file directly
    const response = await fetch(url);
    if (!response.ok) {
        throw new Error(`Failed to fetch asset: ${url} (HTTP ${response.status})`);
    }
    return response.blob();
}
