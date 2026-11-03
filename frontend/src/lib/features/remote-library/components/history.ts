/** When a play happened, from the server's seconds since the epoch: "Oct 3, 09:41". */
export function formatViewedAt(viewedAt: number): string {
	return new Date(viewedAt * 1000).toLocaleDateString(undefined, {
		month: 'short',
		day: 'numeric',
		hour: '2-digit',
		minute: '2-digit'
	});
}
