import { latestRelease, RELEASES_URL } from "../../lib/release";

// /download always sends the newest ISO, looked up on each request.
export const dynamic = "force-dynamic";

export async function GET() {
  const release = await latestRelease();
  return Response.redirect(release ? release.url : RELEASES_URL, 302);
}
