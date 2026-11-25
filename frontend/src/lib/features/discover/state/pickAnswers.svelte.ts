// The Discover page while it is mounted: what its counter says about a pick
// the sampler is about to play. It registers itself on mount and clears
// itself on leaving, so a registration also says the page is showing.

/** The mounted Discover page's counter, by pick key. */
export interface PickAnswerer {
	/** Whether the pick is still unanswered on the counter. */
	open(key: string): boolean;
}

let answerer = $state<PickAnswerer | null>(null);

export const pickAnswers = {
	/** The mounted page's counter, or null while it is not mounted. */
	get current(): PickAnswerer | null {
		return answerer;
	},
	/** Registers the mounted page; returns the clear for its unmount. */
	register(page: PickAnswerer): () => void {
		answerer = page;
		return () => {
			if (answerer === page) answerer = null;
		};
	}
};
