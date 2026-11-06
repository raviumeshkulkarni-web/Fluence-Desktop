import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog';
import { Button } from '@/components/ui/button';
import { parseReleaseNotes } from '@/lib/release-notes';
import { updaterStore, useUpdater } from '@/ipc/updater';

export function UpdateNotesDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const updater = useUpdater();
  const sections = parseReleaseNotes(updater.body);

  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent className="update-notes-dialog">
        <AlertDialogHeader>
          <AlertDialogTitle>
            What’s new{updater.version ? ` in Fluence v${updater.version}` : ''}
          </AlertDialogTitle>
          <AlertDialogDescription>
            Review these release notes before downloading the update.
          </AlertDialogDescription>
        </AlertDialogHeader>

        {sections.length > 0 ? (
          <div
            className="update-notes-scroll"
            role="region"
            aria-label="Release notes"
            tabIndex={0}
          >
            {sections.map((section, index) => (
              <section className="update-notes-section" key={`${section.title}-${index}`}>
                <h3>{section.title}</h3>
                <ul>
                  {section.items.map((item, itemIndex) => (
                    <li key={`${item}-${itemIndex}`}>{item}</li>
                  ))}
                </ul>
              </section>
            ))}
          </div>
        ) : (
          <p className="update-notes-empty">
            The publisher did not include release notes for this update. You can still download it.
          </p>
        )}

        <AlertDialogFooter>
          <AlertDialogCancel asChild>
            <Button variant="secondary">Not now</Button>
          </AlertDialogCancel>
          <AlertDialogAction asChild>
            <Button
              variant="primary"
              onClick={() => void updaterStore.startDownloadAndInstall()}
            >
              Download update
            </Button>
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
