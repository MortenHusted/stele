/* Independent operator baseline: EXEC-only Landlock, not production containment. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/landlock.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>
static int rule(int ruleset,const char *path) {
    int fd=open(path,O_PATH|O_CLOEXEC);
    if(fd<0)return -1;
    struct landlock_path_beneath_attr attr={.allowed_access=LANDLOCK_ACCESS_FS_EXECUTE,.parent_fd=fd};
    int result=syscall(SYS_landlock_add_rule,ruleset,LANDLOCK_RULE_PATH_BENEATH,&attr,0);
    close(fd);return result;
}
static int control(const char *tool,const char *loader,int grant_loader,int unapproved) {
    pid_t pid=fork();if(pid<0)return -1;
    if(!pid){
        struct landlock_ruleset_attr attr={.handled_access_fs=LANDLOCK_ACCESS_FS_EXECUTE};
        int fd=syscall(SYS_landlock_create_ruleset,&attr,sizeof(attr),0);
        if(fd<0||rule(fd,tool)||(grant_loader&&rule(fd,loader)))_exit(90);
        if(prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)||syscall(SYS_landlock_restrict_self,fd,0))_exit(91);
        close(fd);
        char *const args[]={(char *)(unapproved?"/usr/bin/true":tool),"--version",NULL};
        char *const env[]={NULL};
        execve(args[0],args,env);_exit(errno==EACCES?77:92);
    }
    int status;if(waitpid(pid,&status,0)!=pid||!WIFEXITED(status))return -1;
    return WEXITSTATUS(status);
}
int main(int argc,char **argv){
    if(argc!=3)return 2;
    int denied=control(argv[1],argv[2],0,0),allowed=control(argv[1],argv[2],1,0),outside=control(argv[1],argv[2],1,1);
    printf("interpreter_execute_missing=%d exact_interpreter_execute=%d unapproved_executable=%d\n",denied,allowed,outside);
    return denied==77&&allowed==0&&outside==77?0:1;
}
