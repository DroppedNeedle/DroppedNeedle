const GENRE_GRADIENTS = [
	'from-rose-500/90 to-pink-700',
	'from-violet-500/90 to-purple-700',
	'from-blue-500/90 to-cyan-700',
	'from-emerald-500/90 to-teal-700',
	'from-amber-500/90 to-orange-700',
	'from-red-500/90 to-rose-700',
	'from-indigo-500/90 to-violet-700',
	'from-cyan-500/90 to-blue-700',
	'from-green-500/90 to-emerald-700',
	'from-orange-500/90 to-amber-700'
] as const;

export function getGenreGradient(name: string): string {
	let hash = 0;
	for (let index = 0; index < name.length; index += 1) {
		hash = (hash * 31 + name.charCodeAt(index)) | 0;
	}
	return GENRE_GRADIENTS[Math.abs(hash) % GENRE_GRADIENTS.length];
}
