import { A } from '@solidjs/router';
import { useDb } from '@spooky-sync/client-solid';
import { Folder } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { useAuth } from '../lib/auth';
import { courseColor } from '../lib/palette';
import { useCourse } from '../lib/query';

/**
 * The course an editor belongs to, at the start of its title bar: its
 * folder and name as the course page shows them, cut short, back to it
 * (to the course page's `tab`, the editor's own list).
 */
export function CourseCrumb(props: { courseKey: string; tab?: 'homework' | 'documents' | 'sites' }) {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const course = useCourse(db, auth.userId, () => `course:${props.courseKey}`);
  return (
    <A
      href={`/courses/${props.courseKey}${props.tab ? `?tab=${props.tab}` : ''}`}
      class="flex min-w-0 max-w-[9rem] shrink items-center gap-2 transition-opacity hover:opacity-70 sm:max-w-[16rem]"
      title={course.data()?.title ?? 'Course'}
    >
      <Folder
        size={20}
        stroke-width={1.6}
        class="h-[18px] w-[18px] shrink-0 sm:h-5 sm:w-5"
        style={{ color: courseColor(course.data()?.color) }}
        fill={courseColor(course.data()?.color)}
        fill-opacity="0.18"
        aria-hidden="true"
      />
      <span class="min-w-0 truncate text-[16px] font-semibold tracking-tight sm:text-[17px]">{course.data()?.title ?? 'Course'}</span>
    </A>
  );
}
