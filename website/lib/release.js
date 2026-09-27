// The newest RyzikOS build, from the repository's GitHub releases. The
// release workflow publishes one for every push to main.

export const REPO = "cladecarvsna-creator/ryzikos";
export const REPO_URL = `https://github.com/${REPO}`;
export const RELEASES_URL = `${REPO_URL}/releases`;

export async function latestRelease() {
  const headers = { Accept: "application/vnd.github+json", "User-Agent": "ryzikos-website" };
  if (process.env.GITHUB_TOKEN) headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  try {
    const res = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
      headers,
      next: { revalidate: 120 },
    });
    if (!res.ok) return null;
    const r = await res.json();
    const iso = (r.assets || []).find((a) => a.name.endsWith(".iso"));
    if (!iso) return null;
    return {
      version: (r.tag_name || "").replace(/^v/, ""),
      name: r.name || r.tag_name,
      date: r.published_at,
      notes: r.body || "",
      url: iso.browser_download_url,
      file: iso.name,
      size: iso.size,
      downloads: (r.assets || []).reduce((n, a) => n + (a.download_count || 0), 0),
      page: r.html_url,
    };
  } catch {
    return null;
  }
}
